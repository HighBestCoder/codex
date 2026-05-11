use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use codex_call_graph_algo::default_caller_index_cache;
use codex_call_graph_perception::{
    detect_project, locate_rust_analyzer, locate_scip_clang, merge, parse_cpp_project,
    parse_project, run_scip, run_scip_clang, MergedProject, ProjectKind, ProjectParse, ScipOptions,
    ScipClangOptions, ScipResult,
};
use codex_call_graph_store::{GraphStore, ParsedFile};
use serde::{Deserialize, Serialize};

use crate::pgs_path::pgs_path_for;

#[derive(Debug, thiserror::Error)]
pub enum MapError {
    #[error("perception failed: {0}")]
    Perception(#[from] codex_call_graph_perception::PerceptionError),
    #[error("store failed: {0}")]
    Store(#[from] codex_call_graph_store::CallGraphStoreError),
    #[error("project_root must be an absolute path: {0}")]
    NotAbsolute(PathBuf),
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct MapRequest {
    pub project_root: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct MapResponse {
    pub project_root: String,
    pub pgs_path: String,
    pub project_kind: String,
    pub files_seen: u32,
    pub files_indexed: u32,
    pub failures: u32,
    pub nodes_added: u32,
    pub edges_added: u32,
    pub scip_used: bool,
    pub scip_skipped_reason: Option<String>,
    pub elapsed_ms: u64,
}

pub fn run_map(req: &MapRequest) -> Result<MapResponse, MapError> {
    let project_root = PathBuf::from(&req.project_root);
    if !project_root.is_absolute() {
        return Err(MapError::NotAbsolute(project_root));
    }
    let pgs_path = pgs_path_for(&project_root);
    std::fs::create_dir_all(&pgs_path).map_err(codex_call_graph_store::CallGraphStoreError::Io)?;

    let started = Instant::now();
    let kind = detect_project(&project_root);
    let simple = parse_for_kind(&project_root, &kind)?;
    let scip_outcome = run_scip_for_kind(&project_root, &kind);
    let (scip_result, scip_used, scip_skipped_reason) = match scip_outcome {
        ScipOutcome::Used(r) => (Some(r), true, None),
        ScipOutcome::Skipped(reason) => (None, false, Some(reason)),
    };
    let total_files_seen = simple.total_files_seen as u32;
    let failures = simple.failures.len() as u32;
    let merged: MergedProject = merge(&project_root, simple, scip_result);

    let store = GraphStore::open_with_retry(&pgs_path, 25, 200)?;
    let mut nodes_added = 0u32;
    let mut edges_added = 0u32;
    let mut files_indexed = 0u32;
    for file in &merged.project_files {
        let absolute = make_absolute(&project_root, &file.file);
        let parsed_file = ParsedFile {
            file: absolute,
            nodes: file.nodes.clone(),
            edges: file.edges.clone(),
        };
        let stats = store.upsert_file(&parsed_file)?;
        nodes_added = nodes_added.saturating_add(stats.added_nodes as u32);
        edges_added = edges_added.saturating_add(stats.added_edges as u32);
        files_indexed = files_indexed.saturating_add(1);
    }
    if let Some(extern_file) = &merged.external_file {
        let stats = store.upsert_file(extern_file)?;
        nodes_added = nodes_added.saturating_add(stats.added_nodes as u32);
        edges_added = edges_added.saturating_add(stats.added_edges as u32);
    }
    store.flush()?;
    default_caller_index_cache().invalidate(&pgs_path);

    Ok(MapResponse {
        project_root: project_root.display().to_string(),
        pgs_path: pgs_path.display().to_string(),
        project_kind: kind.label().to_string(),
        files_seen: total_files_seen,
        files_indexed,
        failures,
        nodes_added,
        edges_added,
        scip_used,
        scip_skipped_reason,
        elapsed_ms: started.elapsed().as_millis() as u64,
    })
}

fn parse_for_kind(
    root: &Path,
    kind: &ProjectKind,
) -> Result<ProjectParse, codex_call_graph_perception::PerceptionError> {
    match kind {
        ProjectKind::Rust { .. } => parse_project(root),
        ProjectKind::CppCmake { .. } | ProjectKind::CppNoCompdb { .. } => parse_cpp_project(root),
        ProjectKind::Mixed(parts) => {
            let mut acc = ProjectParse::default();
            for part in parts {
                let sub = parse_for_kind(root, part)?;
                acc.files.extend(sub.files);
                acc.failures.extend(sub.failures);
                acc.total_files_seen = acc.total_files_seen.saturating_add(sub.total_files_seen);
            }
            Ok(acc)
        }
        ProjectKind::Unknown { .. } => {
            let rust = parse_project(root)?;
            if !rust.files.is_empty() {
                return Ok(rust);
            }
            parse_cpp_project(root)
        }
    }
}

enum ScipOutcome {
    Used(ScipResult),
    Skipped(String),
}

fn run_scip_for_kind(root: &Path, kind: &ProjectKind) -> ScipOutcome {
    match kind {
        ProjectKind::Rust { .. } => run_scip_rust(root),
        ProjectKind::CppCmake { compile_db } => run_scip_cpp(root, Some(compile_db.as_path())),
        ProjectKind::CppNoCompdb { reason } => ScipOutcome::Skipped(format!(
            "scip-clang needs compile_commands.json: {reason}"
        )),
        ProjectKind::Mixed(parts) => {
            for part in parts {
                let outcome = run_scip_for_kind(root, part);
                if matches!(outcome, ScipOutcome::Used(_)) {
                    return outcome;
                }
            }
            ScipOutcome::Skipped("no SCIP indexer matched a sub-kind".to_string())
        }
        ProjectKind::Unknown { reason } => ScipOutcome::Skipped(format!(
            "project kind unknown ({reason}); SCIP path skipped"
        )),
    }
}

fn run_scip_rust(root: &Path) -> ScipOutcome {
    match locate_rust_analyzer(None) {
        Ok(_) => {
            let options = ScipOptions {
                timeout: Duration::from_secs(600),
                rust_analyzer: None,
                output_path: None,
                exclude_vendored_libraries: true,
            };
            match run_scip(root, &options) {
                Ok(result) => ScipOutcome::Used(result),
                Err(err) => ScipOutcome::Skipped(format!("rust-analyzer scip failed: {err}")),
            }
        }
        Err(err) => ScipOutcome::Skipped(format!("rust-analyzer not on PATH: {err}")),
    }
}

fn run_scip_cpp(root: &Path, compile_db: Option<&Path>) -> ScipOutcome {
    match locate_scip_clang(None) {
        Ok(_) => {
            let options = ScipClangOptions {
                timeout: Duration::from_secs(900),
                binary: None,
                compdb_path: compile_db.map(PathBuf::from),
                output_path: None,
                jobs: None,
            };
            match run_scip_clang(root, &options) {
                Ok(result) => ScipOutcome::Used(result),
                Err(err) => ScipOutcome::Skipped(format!("scip-clang failed: {err}")),
            }
        }
        Err(err) => ScipOutcome::Skipped(format!("scip-clang not on PATH: {err}")),
    }
}

fn make_absolute(root: &Path, file: &Path) -> PathBuf {
    if file.is_absolute() {
        file.to_path_buf()
    } else {
        root.join(file)
    }
}
