mod map;
mod pgs_path;
mod plan;
mod trace;
mod why;

pub use map::{MapError, MapRequest, MapResponse, run_map};
pub use pgs_path::pgs_path_for;
pub use plan::{PlanError, PlanRequest, PlanResponse, SymbolPoint, UnresolvedCallee, run_plan};
pub use trace::{TraceError, TraceRequest, TraceResponse, run_trace, DYNAMIC_TRACE_FILE};
pub use why::{EdgeRef, WhyError, WhyMatch, WhyRequest, WhyResponse, run_why};

#[cfg(test)]
mod e2e_tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    fn write_rust_file(root: &std::path::Path, rel: &str, body: &str) {
        let path = root.join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create dir");
        }
        fs::write(&path, body).expect("write");
    }

    fn project_with_two_rust_files() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        write_rust_file(
            dir.path(),
            "src/lib.rs",
            "pub fn double(x: i32) -> i32 { x + x }\n\
             pub fn quadruple(x: i32) -> i32 { double(double(x)) }\n",
        );
        write_rust_file(
            dir.path(),
            "src/main.rs",
            "fn main() { let _ = quadruple(3); }\n\
             fn quadruple(x: i32) -> i32 { x * 4 }\n",
        );
        dir
    }

    #[test]
    fn run_map_then_run_why_returns_callers_and_callees() {
        let dir = project_with_two_rust_files();
        let project_root = dir
            .path()
            .canonicalize()
            .expect("canonicalize tempdir")
            .display()
            .to_string();

        let map_resp = run_map(&MapRequest {
            project_root: project_root.clone(),
        })
        .expect("map");
        assert!(map_resp.nodes_added >= 4, "expected >=4 nodes, got {map_resp:?}");
        assert!(
            map_resp.files_indexed >= 2,
            "expected >=2 files indexed, got {map_resp:?}"
        );

        let why_resp = run_why(&WhyRequest {
            project_root,
            symbol: "double".into(),
            max_callers: Some(50),
            max_callees: Some(50),
        })
        .expect("why");

        assert!(
            !why_resp.matches.is_empty(),
            "expected at least one match for 'double'"
        );
        let first = &why_resp.matches[0];
        assert_eq!(first.symbol, "double");
        let has_caller_named_quadruple = first
            .callers
            .iter()
            .any(|c| c.symbol == "quadruple");
        assert!(
            has_caller_named_quadruple,
            "expected 'quadruple' as a caller of 'double', got {:?}",
            first.callers
        );
    }

    #[test]
    fn run_why_returns_pgs_missing_when_map_never_ran() {
        let dir = tempfile::tempdir().expect("tempdir");
        let project_root = dir.path().canonicalize().expect("canon").display().to_string();
        let result = run_why(&WhyRequest {
            project_root,
            symbol: "anything".into(),
            max_callers: None,
            max_callees: None,
        });
        match result {
            Err(WhyError::PgsMissing { .. }) => {}
            other => panic!("expected PgsMissing, got {other:?}"),
        }
    }

    #[test]
    fn run_map_rejects_relative_project_root() {
        let result = run_map(&MapRequest {
            project_root: "relative/path".into(),
        });
        match result {
            Err(MapError::NotAbsolute(p)) => {
                assert_eq!(p, PathBuf::from("relative/path"));
            }
            other => panic!("expected NotAbsolute, got {other:?}"),
        }
    }

    #[test]
    fn run_plan_returns_subgraph_covering_seeds() {
        let dir = project_with_two_rust_files();
        let project_root = dir.path().canonicalize().expect("canon").display().to_string();
        run_map(&MapRequest {
            project_root: project_root.clone(),
        })
        .expect("map");

        let resp = run_plan(&PlanRequest {
            project_root,
            seed_symbols: vec!["double".into(), "quadruple".into()],
            max_path_depth: Some(6),
            max_subgraph_size: Some(50),
        })
        .expect("plan");

        assert!(
            resp.seeds_resolved.iter().any(|p| p.symbol == "double"),
            "expected 'double' in seeds_resolved, got {:?}",
            resp.seeds_resolved
        );
        assert!(
            resp.seeds_resolved.iter().any(|p| p.symbol == "quadruple"),
            "expected 'quadruple' in seeds_resolved"
        );
        assert!(resp.seeds_unresolved.is_empty());
        let subgraph_symbols: Vec<&str> = resp.subgraph.iter().map(|p| p.symbol.as_str()).collect();
        assert!(
            subgraph_symbols.contains(&"double") && subgraph_symbols.contains(&"quadruple"),
            "subgraph must cover both seeds, got {subgraph_symbols:?}"
        );
    }

    #[test]
    fn run_plan_reports_unresolved_seeds_without_failing() {
        let dir = project_with_two_rust_files();
        let project_root = dir.path().canonicalize().expect("canon").display().to_string();
        run_map(&MapRequest {
            project_root: project_root.clone(),
        })
        .expect("map");

        let resp = run_plan(&PlanRequest {
            project_root,
            seed_symbols: vec!["double".into(), "does_not_exist".into()],
            max_path_depth: None,
            max_subgraph_size: None,
        })
        .expect("plan");

        assert!(resp.seeds_resolved.iter().any(|p| p.symbol == "double"));
        assert_eq!(resp.seeds_unresolved, vec!["does_not_exist".to_string()]);
    }

    #[test]
    fn run_plan_rejects_empty_seeds() {
        let dir = tempfile::tempdir().expect("dir");
        let result = run_plan(&PlanRequest {
            project_root: dir.path().canonicalize().expect("canon").display().to_string(),
            seed_symbols: Vec::new(),
            max_path_depth: None,
            max_subgraph_size: None,
        });
        match result {
            Err(PlanError::NoSeeds) => {}
            other => panic!("expected NoSeeds, got {other:?}"),
        }
    }

    #[test]
    fn run_plan_surfaces_unresolved_callee_names_with_locations() {
        let dir = tempfile::tempdir().expect("dir");
        let lib_path = dir.path().join("src/lib.rs");
        fs::create_dir_all(lib_path.parent().expect("parent")).expect("mkdir");
        fs::write(
            &lib_path,
            "pub fn caller() {\n    external_helper(1);\n    another_external(2, 3);\n}\n",
        )
        .expect("write");
        let project_root = dir
            .path()
            .canonicalize()
            .expect("canon")
            .display()
            .to_string();
        run_map(&MapRequest {
            project_root: project_root.clone(),
        })
        .expect("map");

        let resp = run_plan(&PlanRequest {
            project_root,
            seed_symbols: vec!["caller".into()],
            max_path_depth: Some(3),
            max_subgraph_size: Some(20),
        })
        .expect("plan");

        let unresolved: Vec<&str> = resp
            .unresolved_callees
            .iter()
            .map(|u| u.callee_name.as_str())
            .collect();
        assert!(
            unresolved.contains(&"external_helper"),
            "expected 'external_helper' in unresolved_callees, got {unresolved:?}"
        );
        assert!(
            unresolved.contains(&"another_external"),
            "expected 'another_external' in unresolved_callees, got {unresolved:?}"
        );
        assert!(!resp.unresolved_callees_truncated);
        for entry in &resp.unresolved_callees {
            assert_eq!(entry.caller_symbol, "caller");
            assert!(
                entry.call_line >= 1,
                "call_line must be >= 1, got {}",
                entry.call_line
            );
        }
    }

    #[test]
    fn run_trace_rejects_empty_command() {
        let dir = tempfile::tempdir().expect("dir");
        let project_root = dir
            .path()
            .canonicalize()
            .expect("canon")
            .display()
            .to_string();
        let err = run_trace(&TraceRequest {
            project_root,
            test_command: Vec::new(),
        })
        .expect_err("empty command should fail");
        assert!(matches!(err, TraceError::EmptyCommand), "got {err:?}");
    }

    #[test]
    fn run_trace_rejects_relative_project_root() {
        let err = run_trace(&TraceRequest {
            project_root: "relative".into(),
            test_command: vec!["true".into()],
        })
        .expect_err("relative path should fail");
        match err {
            TraceError::NotAbsolute(p) => assert_eq!(p, PathBuf::from("relative")),
            other => panic!("expected NotAbsolute, got {other:?}"),
        }
    }

    #[test]
    fn run_trace_reports_pgs_missing_when_map_never_ran() {
        let dir = tempfile::tempdir().expect("tempdir");
        let project_root = dir
            .path()
            .canonicalize()
            .expect("canon")
            .display()
            .to_string();
        let err = run_trace(&TraceRequest {
            project_root,
            test_command: vec!["true".into()],
        })
        .expect_err("missing pgs should fail");
        assert!(matches!(err, TraceError::PgsMissing { .. }), "got {err:?}");
    }
}
