use std::path::{Path, PathBuf};
use std::time::Instant;

use codex_call_graph_perception::{
    detect_project, parse_cpp_project, parse_project, ProjectKind, ProjectParse,
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
    let parsed = parse_for_kind(&project_root, &kind)?;
    let store = GraphStore::open_with_retry(&pgs_path, 25, 200)?;

    let mut nodes_added = 0u32;
    let mut edges_added = 0u32;
    let mut files_indexed = 0u32;
    for file in &parsed.files {
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
    store.flush()?;

    Ok(MapResponse {
        project_root: project_root.display().to_string(),
        pgs_path: pgs_path.display().to_string(),
        project_kind: kind.label().to_string(),
        files_seen: parsed.total_files_seen as u32,
        files_indexed,
        failures: parsed.failures.len() as u32,
        nodes_added,
        edges_added,
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

fn make_absolute(root: &Path, file: &Path) -> PathBuf {
    if file.is_absolute() {
        file.to_path_buf()
    } else {
        root.join(file)
    }
}
