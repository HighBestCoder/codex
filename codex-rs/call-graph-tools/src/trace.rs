use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use codex_call_graph_perception::{
    build_runtime_so, demangle_cpp_simple, read_events_jsonl, reconstruct_from_events,
    FinstrumentBuildOptions, FinstrumentError, ReconstructError,
};
use codex_call_graph_store::{
    CallGraphStoreError, EdgeSource, GraphStore, ParsedFile, RawCallEdge, RawTarget,
};
use serde::{Deserialize, Serialize};

use crate::pgs_path::pgs_path_for;

pub const DYNAMIC_TRACE_FILE: &str = "<dynamic-trace>";

#[derive(Debug, thiserror::Error)]
pub enum TraceError {
    #[error("project_root must be absolute: {0}")]
    NotAbsolute(PathBuf),
    #[error("no PGS at {path}; run graph_map first")]
    PgsMissing { path: PathBuf },
    #[error("empty test_command; provide at least one argument (the program to run)")]
    EmptyCommand,
    #[error("failed to build instrumentation runtime: {0}")]
    Finstrument(#[from] FinstrumentError),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("event reconstruction failed: {0}")]
    Reconstruct(#[from] ReconstructError),
    #[error("store error: {0}")]
    Store(#[from] CallGraphStoreError),
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TraceRequest {
    pub project_root: String,
    #[schemars(length(min = 1, max = 64))]
    pub test_command: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TraceResponse {
    pub project_root: String,
    pub runtime_so: String,
    pub events_path: String,
    pub event_count: u64,
    pub unique_edges: u64,
    pub edges_written: u64,
    pub test_exit_code: i32,
    pub elapsed_ms: u64,
}

pub fn run_trace(req: &TraceRequest) -> Result<TraceResponse, TraceError> {
    if req.test_command.is_empty() {
        return Err(TraceError::EmptyCommand);
    }
    let project_root = PathBuf::from(&req.project_root);
    if !project_root.is_absolute() {
        return Err(TraceError::NotAbsolute(project_root));
    }
    let pgs_path = pgs_path_for(&project_root);
    if !pgs_path.exists() {
        return Err(TraceError::PgsMissing { path: pgs_path });
    }

    let started = Instant::now();
    let cache_dir = project_root.join(".codex-call-graph-cache");
    std::fs::create_dir_all(&cache_dir)?;
    let so_path = build_runtime_so(&cache_dir, &FinstrumentBuildOptions::default())?;

    let events_path = cache_dir.join("trace-events.jsonl");
    let _ = std::fs::remove_file(&events_path);

    let (program, args) = req.test_command.split_first().expect("non-empty checked");
    let status = Command::new(program)
        .args(args)
        .current_dir(&project_root)
        .env("LD_PRELOAD", &so_path)
        .env("CLAW_TRACE_OUTPUT", &events_path)
        .status()?;

    let events = read_events_jsonl(&events_path)?;
    let event_count = events.len() as u64;
    let reconstructed = reconstruct_from_events(&events);
    let unique_edges = reconstructed.edges.len() as u64;

    let store = GraphStore::open_with_retry(&pgs_path, 25, 200)?;
    let raw_edges: Vec<RawCallEdge> = reconstructed
        .edges
        .iter()
        .map(|edge| RawCallEdge {
            caller_simple_name: demangle_cpp_simple(&edge.caller),
            caller_line: 0,
            line: 0,
            source: EdgeSource::Dynamic,
            confidence: EdgeSource::Dynamic.default_confidence(),
            target: RawTarget::Simple {
                callee_simple_name: demangle_cpp_simple(&edge.callee),
            },
        })
        .collect();
    let edges_written = raw_edges.len() as u64;
    let parsed = ParsedFile {
        file: PathBuf::from(DYNAMIC_TRACE_FILE),
        nodes: Vec::new(),
        edges: raw_edges,
    };
    store.upsert_file(&parsed)?;
    store.flush()?;

    Ok(TraceResponse {
        project_root: project_root.display().to_string(),
        runtime_so: so_path.display().to_string(),
        events_path: events_path.display().to_string(),
        event_count,
        unique_edges,
        edges_written,
        test_exit_code: status.code().unwrap_or(-1),
        elapsed_ms: started.elapsed().as_millis() as u64,
    })
}

#[allow(dead_code)]
fn _ensure_path_lives(_: &Path) {}
