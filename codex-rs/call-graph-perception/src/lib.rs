mod error;
pub mod dynamic_finstrument;
pub mod dynamic_inject;
pub mod dynamic_reconstruct;
pub mod dynamic_worktree;
pub mod history;
pub mod merger;
pub mod project;
pub mod static_scip;
pub mod static_scip_clang;
pub mod static_simple;
pub mod static_treesitter_cpp;

pub use dynamic_finstrument::{
    build_runtime_so, finstrument_source, write_runtime_source, BuildOptions as FinstrumentBuildOptions,
    FinstrumentError,
};
pub use dynamic_inject::{
    inject_file, inject_source, inject_workspace, FileInjectResult, InjectError, InjectOptions,
    InjectStats, InjectorStats,
};
pub use dynamic_reconstruct::{
    demangle_cpp_simple, read_events_jsonl, reconstruct_from_events, CallTreeNode, DynEdgeRecord,
    DynEvent, ReconstructError, ReconstructResult,
};
pub use dynamic_worktree::{TraceWorktree, WorktreeError};
pub use error::PerceptionError;
pub use history::{blame_history, count_recent_reverts, open_repo, HistoryError, NodeHistory};
pub use merger::{merge, MergeStats, MergedProject};
pub use project::{detect as detect_project, ProjectKind};
pub use static_scip::{
    locate_rust_analyzer, parse_scip_file, run_scip, ExternalCallee, ScipEdge, ScipFn,
    ScipOptions, ScipResult, ScipStats,
};
pub use static_scip_clang::{locate_scip_clang, run_scip_clang, ScipClangOptions};
pub use static_simple::{parse_file, parse_project, ProjectParse};
pub use static_treesitter_cpp::{parse_cpp_file, parse_cpp_project, CppDialect};

#[cfg(test)]
mod tests {
    use super::*;
    use codex_call_graph_store::{EdgeSource, RawTarget};
    use std::fs;
    use tempfile::tempdir;
    fn write_rust(dir: &std::path::Path, name: &str, body: &str) -> std::path::PathBuf {
        let path = dir.join(name);
        fs::write(&path, body).expect("write");
        path
    }

    #[test]
    fn parses_free_fns_with_simple_calls() {
        let dir = tempdir().expect("tempdir");
        let path = write_rust(
            dir.path(),
            "lib.rs",
            "fn add(a: i64, b: i64) -> i64 { a + b }\n\
             fn double(x: i64) -> i64 { add(x, x) }\n\
             fn quadruple(x: i64) -> i64 { let d = double(x); double(d) }\n",
        );
        let parsed = parse_file(&path).expect("parse");
        let names: Vec<&str> = parsed.nodes.iter().map(|n| n.simple_name.as_str()).collect();
        assert_eq!(names, vec!["add", "double", "quadruple"]);

        let edges_by_pair: Vec<(String, String)> = parsed
            .edges
            .iter()
            .map(|e| match &e.target {
                RawTarget::Simple { callee_simple_name } => {
                    (e.caller_simple_name.clone(), callee_simple_name.clone())
                }
                _ => panic!("expected simple-name target"),
            })
            .collect();
        assert!(edges_by_pair.contains(&("double".into(), "add".into())));
        assert_eq!(
            edges_by_pair
                .iter()
                .filter(|(c, t)| c == "quadruple" && t == "double")
                .count(),
            2
        );
    }

    #[test]
    fn parses_impl_methods_and_async_fns() {
        let dir = tempdir().expect("tempdir");
        let path = write_rust(
            dir.path(),
            "lib.rs",
            "pub struct Greeter; \
             impl Greeter { pub fn greet(&self, n: &str) -> String { format!(\"hi {n}\") } } \
             pub async fn fetch(seed: u64) -> u64 { seed }",
        );
        let parsed = parse_file(&path).expect("parse");
        let greet = parsed
            .nodes
            .iter()
            .find(|n| n.simple_name == "greet")
            .expect("greet");
        assert!(greet.is_method);
        assert!(!greet.is_async);
        let fetch = parsed
            .nodes
            .iter()
            .find(|n| n.simple_name == "fetch")
            .expect("fetch");
        assert!(fetch.is_async);
        assert!(!fetch.is_method);
    }

    #[test]
    fn captures_method_calls_as_simple_edges() {
        let dir = tempdir().expect("tempdir");
        let path = write_rust(
            dir.path(),
            "lib.rs",
            "fn ping(s: &str) -> usize { s.trim().len() }",
        );
        let parsed = parse_file(&path).expect("parse");
        let names: Vec<String> = parsed
            .edges
            .iter()
            .filter(|e| e.caller_simple_name == "ping")
            .map(|e| match &e.target {
                RawTarget::Simple { callee_simple_name } => callee_simple_name.clone(),
                _ => unreachable!(),
            })
            .collect();
        assert!(names.contains(&"trim".to_string()));
        assert!(names.contains(&"len".to_string()));
    }

    #[test]
    fn edge_source_default_is_simple_name() {
        let dir = tempdir().expect("tempdir");
        let path = write_rust(
            dir.path(),
            "lib.rs",
            "fn one() -> i64 { 1 }\nfn two() -> i64 { one() + one() }",
        );
        let parsed = parse_file(&path).expect("parse");
        for edge in &parsed.edges {
            assert_eq!(edge.source, EdgeSource::SimpleName);
            assert!((edge.confidence - 0.85).abs() < 1e-6);
        }
    }

    #[test]
    fn parse_project_walks_directory_in_parallel() {
        let dir = tempdir().expect("tempdir");
        write_rust(dir.path(), "a.rs", "fn x() {}");
        write_rust(dir.path(), "b.rs", "fn y() { z(); }");
        let nested = dir.path().join("nested");
        fs::create_dir_all(&nested).expect("nested");
        write_rust(&nested, "c.rs", "fn z() {}");

        let project = parse_project(dir.path()).expect("project");
        assert_eq!(project.total_files_seen, 3);
        assert_eq!(project.files.len(), 3);
        assert!(project.failures.is_empty());
        let total_nodes: usize = project.files.iter().map(|f| f.nodes.len()).sum();
        assert_eq!(total_nodes, 3);
    }

    #[test]
    fn parse_project_records_failures_without_aborting() {
        let dir = tempdir().expect("tempdir");
        write_rust(dir.path(), "good.rs", "fn ok() {}");
        write_rust(dir.path(), "bad.rs", "fn !!!");
        let project = parse_project(dir.path()).expect("project");
        assert_eq!(project.total_files_seen, 2);
        assert_eq!(project.files.len(), 1);
        assert_eq!(project.failures.len(), 1);
        assert_eq!(project.files[0].nodes[0].simple_name, "ok");
    }

    #[test]
    fn parse_project_skips_target_directories() {
        let dir = tempdir().expect("tempdir");
        let target = dir.path().join("target").join("debug");
        fs::create_dir_all(&target).expect("target");
        write_rust(&target, "junk.rs", "fn junk() {}");
        write_rust(dir.path(), "real.rs", "fn real() {}");
        let project = parse_project(dir.path()).expect("project");
        assert_eq!(project.total_files_seen, 1);
        assert_eq!(project.files[0].nodes[0].simple_name, "real");
    }

    #[test]
    fn locate_rust_analyzer_explicit_path_must_exist() {
        let dir = tempdir().expect("tempdir");
        let bogus = dir.path().join("does-not-exist");
        let result = locate_rust_analyzer(Some(&bogus));
        assert!(result.is_err());
    }

    #[test]
    fn locate_rust_analyzer_accepts_existing_explicit_file() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("rust-analyzer");
        fs::write(&path, b"#!/bin/sh\nexit 0\n").expect("write stub");
        let resolved = locate_rust_analyzer(Some(&path)).expect("locate");
        assert_eq!(resolved, path);
    }

    #[test]
    fn parse_scip_file_extracts_functions_and_edges() {
        let scip_path = std::path::Path::new("/tmp/s3.scip");
        if !scip_path.exists() {
            eprintln!("skip: /tmp/s3.scip is not present (run S3 spike first to populate)");
            return;
        }
        let started = std::time::Instant::now();
        let result = parse_scip_file(scip_path, started).expect("parse scip");
        assert!(result.functions.len() >= 4, "expected at least 4 fns, got {}", result.functions.len());
        let names: Vec<&str> = result.functions.iter().map(|f| f.display.as_str()).collect();
        assert!(names.iter().any(|n| n.contains("add")));
        assert!(names.iter().any(|n| n.contains("double")));
        assert!(result.edges.iter().any(|e| e.callee_symbol.contains("double")));
    }

    #[test]
    fn merge_passes_through_simple_when_scip_is_none() {
        let dir = tempdir().expect("tempdir");
        write_rust(
            dir.path(),
            "lib.rs",
            "fn a() { b() }\nfn b() {}\n",
        );
        let simple = parse_project(dir.path()).expect("simple");
        let merged = merger::merge(dir.path(), simple, None);
        assert!(merged.external_file.is_none());
        assert_eq!(merged.project_files.len(), 1);
        assert_eq!(merged.stats.scip_internal_edges_added, 0);
        assert!(merged.stats.simple_name_edges_kept >= 1);
    }

    #[test]
    fn merge_drops_simple_name_edge_when_scip_covers_same_pair() {
        use codex_call_graph_store::ParsedFile as PFile;
        use static_scip::{ExternalCallee as Ec, ScipEdge, ScipFn, ScipResult, ScipStats};
        use static_simple::ProjectParse;

        let project_root = std::path::Path::new("/proj");
        let file = std::path::PathBuf::from("/proj/src/lib.rs");

        let simple_parsed = PFile {
            file: file.clone(),
            nodes: vec![codex_call_graph_store::RawFnNode {
                simple_name: "double".into(),
                line: 1, is_async: false, is_method: false, loc: 1,
                fingerprint: [0u8; 16],
            }],
            edges: vec![codex_call_graph_store::RawCallEdge {
                caller_simple_name: "double".into(),
                caller_line: 0,
                line: 2,
                source: codex_call_graph_store::EdgeSource::SimpleName,
                confidence: 0.85,
                target: codex_call_graph_store::RawTarget::Simple { callee_simple_name: "add".into() },
            }],
        };
        let simple = ProjectParse {
            files: vec![simple_parsed],
            failures: vec![],
            total_files_seen: 1,
        };

        let scip = ScipResult {
            functions: vec![
                ScipFn { symbol: "rust-analyzer cargo proj 0.1.0 double().".into(),
                         display: "double".into(),
                         file: std::path::PathBuf::from("src/lib.rs"),
                         line: 1 },
                ScipFn { symbol: "rust-analyzer cargo proj 0.1.0 add().".into(),
                         display: "add".into(),
                         file: std::path::PathBuf::from("src/lib.rs"),
                         line: 5 },
            ],
            external_callees: vec![],
            edges: vec![ScipEdge {
                caller_symbol: "rust-analyzer cargo proj 0.1.0 double().".into(),
                callee_symbol: "rust-analyzer cargo proj 0.1.0 add().".into(),
                line: 2,
                is_external: false,
            }],
            stats: ScipStats::default(),
        };

        let merged = merger::merge(project_root, simple, Some(scip));
        assert_eq!(merged.stats.simple_name_edges_dropped, 1);
        assert_eq!(merged.stats.scip_internal_edges_added, 1);
        let edges = &merged.project_files[0].edges;
        assert_eq!(edges.len(), 1, "exactly one edge after dedup");
        assert_eq!(edges[0].source, codex_call_graph_store::EdgeSource::ScipInternal);
        assert!((edges[0].confidence - 0.99).abs() < 1e-6);
    }

    #[test]
    fn merge_promotes_external_callees_into_synthetic_extern_file() {
        use static_scip::{ExternalCallee as Ec, ScipEdge, ScipFn, ScipResult, ScipStats};
        use static_simple::ProjectParse;

        let project_root = std::path::Path::new("/proj");
        let scip = ScipResult {
            functions: vec![ScipFn {
                symbol: "rust-analyzer cargo proj 0.1.0 caller().".into(),
                display: "caller".into(),
                file: std::path::PathBuf::from("src/lib.rs"),
                line: 1,
            }],
            external_callees: vec![Ec {
                symbol: "rust-analyzer cargo std 1.87.0 sync/Mutex#new().".into(),
                display: "sync/Mutex#new()".into(),
                crate_hint: Some("std".into()),
            }],
            edges: vec![ScipEdge {
                caller_symbol: "rust-analyzer cargo proj 0.1.0 caller().".into(),
                callee_symbol: "rust-analyzer cargo std 1.87.0 sync/Mutex#new().".into(),
                line: 7,
                is_external: true,
            }],
            stats: ScipStats::default(),
        };

        let simple = ProjectParse {
            files: vec![codex_call_graph_store::ParsedFile {
                file: std::path::PathBuf::from("/proj/src/lib.rs"),
                nodes: vec![codex_call_graph_store::RawFnNode {
                    simple_name: "caller".into(),
                    line: 1,
                    is_async: false,
                    is_method: false,
                    loc: 1,
                    fingerprint: [0u8; 16],
                }],
                edges: vec![],
            }],
            failures: vec![],
            total_files_seen: 1,
        };

        let merged = merger::merge(project_root, simple, Some(scip));
        assert_eq!(merged.stats.scip_external_edges_added, 1);
        assert_eq!(merged.stats.external_node_count, 1);
        let extern_file = merged.external_file.expect("external file present");
        assert_eq!(extern_file.file, std::path::PathBuf::from("<extern>"));
        assert_eq!(extern_file.nodes.len(), 1);
        let extern_edges = &merged.project_files[0].edges;
        assert_eq!(extern_edges.len(), 1);
        assert_eq!(extern_edges[0].source, codex_call_graph_store::EdgeSource::ScipExternal);
        match &extern_edges[0].target {
            codex_call_graph_store::RawTarget::Simple { callee_simple_name } => {
                assert!(
                    callee_simple_name.ends_with("sync/Mutex#new()."),
                    "external callee target should be the SCIP symbol verbatim, got {callee_simple_name}",
                );
            }
            _ => panic!("expected simple-target callee for external edge"),
        }
    }
}
