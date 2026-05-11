use std::collections::HashSet;
use std::path::PathBuf;

use codex_call_graph_algo::{steiner_subgraph, topo_sort, GraphAlgoError, LoadedGraph};
use codex_call_graph_store::{FnNode, GraphStore, NodeId};
use serde::{Deserialize, Serialize};

use crate::pgs_path::pgs_path_for;

#[derive(Debug, thiserror::Error)]
pub enum PlanError {
    #[error("store failed: {0}")]
    Store(#[from] codex_call_graph_store::CallGraphStoreError),
    #[error("graph algo failed: {0}")]
    Algo(#[from] GraphAlgoError),
    #[error("project_root must be absolute: {0}")]
    NotAbsolute(PathBuf),
    #[error("no PGS at {path}; run graph_map first")]
    PgsMissing { path: PathBuf },
    #[error("no seed_symbols supplied")]
    NoSeeds,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PlanRequest {
    pub project_root: String,
    #[schemars(length(min = 1, max = 32))]
    pub seed_symbols: Vec<String>,
    #[schemars(range(min = 1, max = 20))]
    pub max_path_depth: Option<usize>,
    #[schemars(range(min = 1, max = 500))]
    pub max_subgraph_size: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PlanResponse {
    pub seeds_resolved: Vec<SymbolPoint>,
    pub seeds_unresolved: Vec<String>,
    pub subgraph: Vec<SymbolPoint>,
    pub topo_order: Vec<SymbolPoint>,
    pub cycle_detected: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SymbolPoint {
    pub symbol: String,
    pub file: String,
    pub line: u32,
}

pub fn run_plan(req: &PlanRequest) -> Result<PlanResponse, PlanError> {
    if req.seed_symbols.is_empty() {
        return Err(PlanError::NoSeeds);
    }
    let project_root = PathBuf::from(&req.project_root);
    if !project_root.is_absolute() {
        return Err(PlanError::NotAbsolute(project_root));
    }
    let pgs_path = pgs_path_for(&project_root);
    if !pgs_path.exists() {
        return Err(PlanError::PgsMissing { path: pgs_path });
    }
    let store = GraphStore::open_with_retry(&pgs_path, 25, 200)?;

    let mut seeds_resolved: Vec<SymbolPoint> = Vec::new();
    let mut seed_node_ids: Vec<NodeId> = Vec::new();
    let mut seeds_unresolved: Vec<String> = Vec::new();
    for symbol in &req.seed_symbols {
        let hits = store.nodes_by_name(symbol)?;
        if hits.is_empty() {
            seeds_unresolved.push(symbol.clone());
            continue;
        }
        for hit in hits {
            seed_node_ids.push(hit.id);
            seeds_resolved.push(node_to_point(&hit));
        }
    }

    if seed_node_ids.is_empty() {
        return Ok(PlanResponse {
            seeds_resolved,
            seeds_unresolved,
            subgraph: Vec::new(),
            topo_order: Vec::new(),
            cycle_detected: false,
        });
    }

    let max_depth = req.max_path_depth.unwrap_or(6);
    let max_size = req.max_subgraph_size.unwrap_or(50);
    let loaded = LoadedGraph::from_pgs_bounded(&store, &seed_node_ids, max_depth, max_size)?;
    let covered: HashSet<NodeId> = steiner_subgraph(&loaded, &seed_node_ids, max_depth, max_size);

    let mut subgraph_points: Vec<SymbolPoint> = Vec::with_capacity(covered.len());
    for id in &covered {
        if let Some(node) = store.node(*id)? {
            subgraph_points.push(node_to_point(&node));
        }
    }
    subgraph_points.sort_by(|a, b| a.file.cmp(&b.file).then_with(|| a.line.cmp(&b.line)));

    let (topo_order, cycle_detected) = match topo_sort(&loaded) {
        Ok(ordering) => {
            let mut points = Vec::new();
            for id in ordering {
                if !covered.contains(&id) {
                    continue;
                }
                if let Some(node) = store.node(id)? {
                    points.push(node_to_point(&node));
                }
            }
            (points, false)
        }
        Err(GraphAlgoError::CycleDetected) => (Vec::new(), true),
        Err(other) => return Err(PlanError::Algo(other)),
    };

    Ok(PlanResponse {
        seeds_resolved,
        seeds_unresolved,
        subgraph: subgraph_points,
        topo_order,
        cycle_detected,
    })
}

fn node_to_point(node: &FnNode) -> SymbolPoint {
    SymbolPoint {
        symbol: node.simple_name.clone(),
        file: node.file.display().to_string(),
        line: node.line,
    }
}
