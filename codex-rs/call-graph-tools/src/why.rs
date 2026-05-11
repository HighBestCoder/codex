use std::path::PathBuf;

use codex_call_graph_store::{EdgeSource, GraphStore, ResolvedTarget};
use serde::{Deserialize, Serialize};

use crate::pgs_path::pgs_path_for;

#[derive(Debug, thiserror::Error)]
pub enum WhyError {
    #[error("store failed: {0}")]
    Store(#[from] codex_call_graph_store::CallGraphStoreError),
    #[error("project_root must be absolute: {0}")]
    NotAbsolute(PathBuf),
    #[error("no PGS at {path}; run graph_map first")]
    PgsMissing { path: PathBuf },
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct WhyRequest {
    pub project_root: String,
    pub symbol: String,
    #[schemars(range(min = 1, max = 200))]
    pub max_callers: Option<usize>,
    #[schemars(range(min = 1, max = 200))]
    pub max_callees: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct WhyResponse {
    pub matches: Vec<WhyMatch>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct WhyMatch {
    pub symbol: String,
    pub file: String,
    pub line: u32,
    pub callers: Vec<EdgeRef>,
    pub callees: Vec<EdgeRef>,
    pub callers_truncated: bool,
    pub callees_truncated: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct EdgeRef {
    pub symbol: String,
    pub file: Option<String>,
    pub line: u32,
    pub source: String,
    pub confidence: f32,
}

pub fn run_why(req: &WhyRequest) -> Result<WhyResponse, WhyError> {
    let project_root = PathBuf::from(&req.project_root);
    if !project_root.is_absolute() {
        return Err(WhyError::NotAbsolute(project_root));
    }
    let pgs_path = pgs_path_for(&project_root);
    if !pgs_path.exists() {
        return Err(WhyError::PgsMissing { path: pgs_path });
    }
    let store = GraphStore::open_with_retry(&pgs_path, 25, 200)?;

    let max_callers = req.max_callers.unwrap_or(20);
    let max_callees = req.max_callees.unwrap_or(20);
    let hits = store.nodes_by_name(&req.symbol)?;

    let simple_name_callers = simple_name_callers_for(&store, &req.symbol)?;

    let mut matches = Vec::with_capacity(hits.len());
    for hit in hits {
        let mut raw_callers = store.in_edges(hit.id)?;
        for entry in &simple_name_callers {
            raw_callers.push(entry.clone());
        }

        let raw_callees = store.out_edges(hit.id)?;
        let callers_truncated = raw_callers.len() > max_callers;
        let callees_truncated = raw_callees.len() > max_callees;

        let callers = raw_callers
            .into_iter()
            .take(max_callers)
            .map(|edge| edge_ref_for_caller(&store, &edge))
            .collect::<Result<Vec<_>, _>>()?;
        let callees = raw_callees
            .into_iter()
            .take(max_callees)
            .map(|edge| edge_ref_for_callee(&store, &edge))
            .collect::<Result<Vec<_>, _>>()?;

        matches.push(WhyMatch {
            symbol: hit.simple_name,
            file: hit.file.display().to_string(),
            line: hit.line,
            callers,
            callees,
            callers_truncated,
            callees_truncated,
        });
    }
    Ok(WhyResponse { matches })
}

fn simple_name_callers_for(
    store: &GraphStore,
    symbol: &str,
) -> Result<Vec<codex_call_graph_store::CallEdge>, codex_call_graph_store::CallGraphStoreError> {
    let mut out = Vec::new();
    for edge_result in store.iter_out_edges() {
        let edge = edge_result?;
        if let ResolvedTarget::Simple { callee_name } = &edge.target {
            if callee_name == symbol {
                out.push(edge);
            }
        }
    }
    Ok(out)
}

fn edge_ref_for_caller(
    store: &GraphStore,
    edge: &codex_call_graph_store::CallEdge,
) -> Result<EdgeRef, codex_call_graph_store::CallGraphStoreError> {
    let caller = store.node(edge.caller_id)?;
    let (symbol, file) = match caller {
        Some(node) => (node.simple_name, Some(node.file.display().to_string())),
        None => ("<unknown>".to_string(), None),
    };
    Ok(EdgeRef {
        symbol,
        file,
        line: edge.line,
        source: format_source(edge.source),
        confidence: edge.confidence,
    })
}

fn edge_ref_for_callee(
    store: &GraphStore,
    edge: &codex_call_graph_store::CallEdge,
) -> Result<EdgeRef, codex_call_graph_store::CallGraphStoreError> {
    let (symbol, file) = match &edge.target {
        ResolvedTarget::Simple { callee_name } => (callee_name.clone(), None),
        ResolvedTarget::Resolved { callee_id } => match store.node(*callee_id)? {
            Some(node) => (node.simple_name, Some(node.file.display().to_string())),
            None => ("<unknown>".to_string(), None),
        },
    };
    Ok(EdgeRef {
        symbol,
        file,
        line: edge.line,
        source: format_source(edge.source),
        confidence: edge.confidence,
    })
}

fn format_source(s: EdgeSource) -> String {
    match s {
        EdgeSource::SimpleName => "simple_name",
        EdgeSource::ScipInternal => "scip_internal",
        EdgeSource::ScipExternal => "scip_external",
        EdgeSource::Dynamic => "dynamic",
    }
    .to_string()
}
