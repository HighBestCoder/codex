use std::collections::{HashMap, HashSet, VecDeque};

use codex_call_graph_store::{CallEdge, FnNode, GraphStore, NodeId, CallGraphStoreError, ResolvedTarget};
use petgraph::algo::{kosaraju_scc, toposort};
use petgraph::graph::{DiGraph, NodeIndex};
use petgraph::Direction;

#[derive(Debug, thiserror::Error)]
pub enum GraphAlgoError {
    #[error("pgs error: {0}")]
    Pgs(#[from] CallGraphStoreError),
    #[error("graph contains a cycle: cannot toposort")]
    CycleDetected,
}

pub struct LoadedGraph {
    pub graph: DiGraph<NodeId, EdgeWeight>,
    pub by_node_id: HashMap<NodeId, NodeIndex>,
}

#[derive(Debug, Clone, Copy)]
pub struct EdgeWeight {
    pub source: codex_call_graph_store::EdgeSource,
    pub confidence: f32,
}

impl LoadedGraph {
    pub fn from_pgs(pgs: &GraphStore) -> Result<Self, GraphAlgoError> {
        let mut graph = DiGraph::<NodeId, EdgeWeight>::new();
        let mut by_node_id: HashMap<NodeId, NodeIndex> = HashMap::new();
        let mut by_simple_name: HashMap<String, Vec<NodeId>> = HashMap::new();

        let mut nodes: Vec<FnNode> = Vec::new();
        for n in pgs.iter_nodes() {
            nodes.push(n?);
        }
        for n in &nodes {
            let nx = graph.add_node(n.id);
            by_node_id.insert(n.id, nx);
            by_simple_name
                .entry(n.simple_name.clone())
                .or_default()
                .push(n.id);
        }

        for edge_result in pgs.iter_out_edges() {
            let edge: CallEdge = edge_result?;
            let Some(&caller_ix) = by_node_id.get(&edge.caller_id) else {
                continue;
            };
            let weight = EdgeWeight {
                source: edge.source,
                confidence: edge.confidence,
            };
            match edge.target {
                ResolvedTarget::Resolved { callee_id } => {
                    if let Some(&callee_ix) = by_node_id.get(&callee_id) {
                        graph.add_edge(caller_ix, callee_ix, weight);
                    }
                }
                ResolvedTarget::Simple { callee_name } => {
                    if let Some(callee_ids) = by_simple_name.get(&callee_name) {
                        for &cid in callee_ids {
                            if let Some(&callee_ix) = by_node_id.get(&cid) {
                                graph.add_edge(caller_ix, callee_ix, weight);
                            }
                        }
                    }
                }
            }
        }

        Ok(Self { graph, by_node_id })
    }

    pub fn node_count(&self) -> usize {
        self.graph.node_count()
    }

    pub fn edge_count(&self) -> usize {
        self.graph.edge_count()
    }

    pub fn lookup(&self, id: NodeId) -> Option<NodeIndex> {
        self.by_node_id.get(&id).copied()
    }

    pub fn node_id_at(&self, ix: NodeIndex) -> Option<NodeId> {
        self.graph.node_weight(ix).copied()
    }

    /// Build a graph that only covers nodes within `max_radius` BFS hops of
    /// any seed, capped at `max_nodes` total. Lets callers (e.g. graph_plan
    /// on a 530-file project) run Steiner / topo on a few hundred nodes
    /// instead of the full call graph.
    pub fn from_pgs_bounded(
        pgs: &GraphStore,
        seeds: &[NodeId],
        max_radius: usize,
        max_nodes: usize,
    ) -> Result<Self, GraphAlgoError> {
        let callers_by_callee = build_simple_name_caller_index(pgs)?;

        let mut collected: HashSet<NodeId> = HashSet::new();
        let mut frontier: Vec<NodeId> = Vec::new();
        for &seed in seeds {
            if pgs.node(seed)?.is_some() && collected.insert(seed) {
                frontier.push(seed);
            }
        }

        for _hop in 0..max_radius {
            if collected.len() >= max_nodes || frontier.is_empty() {
                break;
            }
            let mut next_frontier: Vec<NodeId> = Vec::new();
            for node_id in frontier.drain(..) {
                for edge in pgs.out_edges(node_id)? {
                    if let Some(callee_id) = callee_node_id_for_edge(pgs, &edge)?.first().copied() {
                        if collected.insert(callee_id) {
                            next_frontier.push(callee_id);
                            if collected.len() >= max_nodes {
                                break;
                            }
                        }
                    }
                }
                if collected.len() >= max_nodes {
                    break;
                }
                if let Some(callers) = callers_by_callee.get(&node_id) {
                    for &caller in callers {
                        if collected.insert(caller) {
                            next_frontier.push(caller);
                            if collected.len() >= max_nodes {
                                break;
                            }
                        }
                    }
                }
            }
            if collected.len() >= max_nodes {
                break;
            }
            frontier = next_frontier;
        }

        Self::build_from_node_set(pgs, &collected, &callers_by_callee)
    }

    fn build_from_node_set(
        pgs: &GraphStore,
        node_ids: &HashSet<NodeId>,
        callers_by_callee: &HashMap<NodeId, Vec<NodeId>>,
    ) -> Result<Self, GraphAlgoError> {
        let mut graph = DiGraph::<NodeId, EdgeWeight>::new();
        let mut by_node_id: HashMap<NodeId, NodeIndex> = HashMap::new();
        for &id in node_ids {
            let nx = graph.add_node(id);
            by_node_id.insert(id, nx);
        }
        for &caller_id in node_ids {
            let Some(&caller_ix) = by_node_id.get(&caller_id) else {
                continue;
            };
            for edge in pgs.out_edges(caller_id)? {
                let weight = EdgeWeight {
                    source: edge.source,
                    confidence: edge.confidence,
                };
                for callee_id in callee_node_id_for_edge(pgs, &edge)? {
                    if let Some(&callee_ix) = by_node_id.get(&callee_id) {
                        graph.add_edge(caller_ix, callee_ix, weight);
                    }
                }
            }
            let _ = callers_by_callee;
        }
        Ok(Self { graph, by_node_id })
    }
}

fn build_simple_name_caller_index(
    pgs: &GraphStore,
) -> Result<HashMap<NodeId, Vec<NodeId>>, GraphAlgoError> {
    let mut index: HashMap<NodeId, Vec<NodeId>> = HashMap::new();
    for edge_result in pgs.iter_out_edges() {
        let edge: CallEdge = edge_result?;
        let callee_ids = callee_node_id_for_edge(pgs, &edge)?;
        for callee_id in callee_ids {
            index.entry(callee_id).or_default().push(edge.caller_id);
        }
    }
    Ok(index)
}

fn callee_node_id_for_edge(
    pgs: &GraphStore,
    edge: &CallEdge,
) -> Result<Vec<NodeId>, GraphAlgoError> {
    match &edge.target {
        ResolvedTarget::Resolved { callee_id } => Ok(vec![*callee_id]),
        ResolvedTarget::Simple { callee_name } => {
            let hits = pgs.nodes_by_name(callee_name)?;
            Ok(hits.into_iter().map(|n| n.id).collect())
        }
    }
}

pub fn scc_components(g: &LoadedGraph) -> Vec<Vec<NodeId>> {
    kosaraju_scc(&g.graph)
        .into_iter()
        .map(|comp| {
            comp.into_iter()
                .filter_map(|ix| g.graph.node_weight(ix).copied())
                .collect()
        })
        .collect()
}

pub fn largest_scc_size(g: &LoadedGraph) -> usize {
    scc_components(g).into_iter().map(|c| c.len()).max().unwrap_or(0)
}

pub fn topo_sort(g: &LoadedGraph) -> Result<Vec<NodeId>, GraphAlgoError> {
    match toposort(&g.graph, None) {
        Ok(order) => Ok(order
            .into_iter()
            .filter_map(|ix| g.graph.node_weight(ix).copied())
            .collect()),
        Err(_) => Err(GraphAlgoError::CycleDetected),
    }
}

pub fn pagerank(g: &LoadedGraph, damping: f32, iterations: u32) -> HashMap<NodeId, f32> {
    let n = g.node_count();
    if n == 0 {
        return HashMap::new();
    }
    let mut ranks: HashMap<NodeIndex, f32> = HashMap::with_capacity(n);
    let initial = 1.0 / n as f32;
    for ix in g.graph.node_indices() {
        ranks.insert(ix, initial);
    }
    let teleport = (1.0 - damping) / n as f32;
    for _ in 0..iterations {
        let mut next: HashMap<NodeIndex, f32> = HashMap::with_capacity(n);
        for ix in g.graph.node_indices() {
            next.insert(ix, teleport);
        }
        for ix in g.graph.node_indices() {
            let out_count = g.graph.neighbors_directed(ix, Direction::Outgoing).count();
            if out_count == 0 {
                let dangling = ranks.get(&ix).copied().unwrap_or(0.0) * damping / n as f32;
                for nx in g.graph.node_indices() {
                    *next.entry(nx).or_insert(0.0) += dangling;
                }
                continue;
            }
            let share = ranks.get(&ix).copied().unwrap_or(0.0) * damping / out_count as f32;
            for nb in g.graph.neighbors_directed(ix, Direction::Outgoing) {
                *next.entry(nb).or_insert(0.0) += share;
            }
        }
        ranks = next;
    }
    ranks
        .into_iter()
        .filter_map(|(ix, score)| g.graph.node_weight(ix).map(|id| (*id, score)))
        .collect()
}

pub fn shortest_path(
    g: &LoadedGraph,
    source: NodeId,
    target: NodeId,
) -> Option<Vec<NodeId>> {
    bfs_path(g, source, target, usize::MAX)
}

pub fn shortest_path_undirected(
    g: &LoadedGraph,
    a: NodeId,
    b: NodeId,
    max_depth: usize,
) -> Option<Vec<NodeId>> {
    let start = g.lookup(a)?;
    let end = g.lookup(b)?;
    let mut parent: HashMap<NodeIndex, NodeIndex> = HashMap::new();
    parent.insert(start, start);
    let mut q: VecDeque<(NodeIndex, usize)> = VecDeque::new();
    q.push_back((start, 0));
    while let Some((u, d)) = q.pop_front() {
        if u == end {
            let mut path = vec![u];
            let mut cur = u;
            while parent[&cur] != cur {
                cur = parent[&cur];
                path.push(cur);
            }
            path.reverse();
            return Some(
                path.into_iter()
                    .filter_map(|ix| g.graph.node_weight(ix).copied())
                    .collect(),
            );
        }
        if d >= max_depth {
            continue;
        }
        for v in g
            .graph
            .neighbors_directed(u, Direction::Outgoing)
            .chain(g.graph.neighbors_directed(u, Direction::Incoming))
        {
            if let std::collections::hash_map::Entry::Vacant(slot) = parent.entry(v) {
                slot.insert(u);
                q.push_back((v, d + 1));
            }
        }
    }
    None
}

fn bfs_path(
    g: &LoadedGraph,
    source: NodeId,
    target: NodeId,
    max_depth: usize,
) -> Option<Vec<NodeId>> {
    let start = g.lookup(source)?;
    let end = g.lookup(target)?;
    let mut parent: HashMap<NodeIndex, NodeIndex> = HashMap::new();
    parent.insert(start, start);
    let mut q: VecDeque<(NodeIndex, usize)> = VecDeque::new();
    q.push_back((start, 0));
    while let Some((u, d)) = q.pop_front() {
        if u == end {
            let mut path = vec![u];
            let mut cur = u;
            while parent[&cur] != cur {
                cur = parent[&cur];
                path.push(cur);
            }
            path.reverse();
            return Some(
                path.into_iter()
                    .filter_map(|ix| g.graph.node_weight(ix).copied())
                    .collect(),
            );
        }
        if d >= max_depth {
            continue;
        }
        for v in g.graph.neighbors_directed(u, Direction::Outgoing) {
            if let std::collections::hash_map::Entry::Vacant(slot) = parent.entry(v) {
                slot.insert(u);
                q.push_back((v, d + 1));
            }
        }
    }
    None
}

pub fn steiner_subgraph(
    g: &LoadedGraph,
    seeds: &[NodeId],
    max_path_depth: usize,
    max_subgraph_size: usize,
) -> HashSet<NodeId> {
    let mut covered: HashSet<NodeId> = HashSet::new();
    let resolved: Vec<NodeId> = seeds
        .iter()
        .copied()
        .filter(|id| g.lookup(*id).is_some())
        .collect();
    for &id in &resolved {
        covered.insert(id);
    }
    'outer: for i in 0..resolved.len() {
        for j in (i + 1)..resolved.len() {
            if covered.len() >= max_subgraph_size {
                break 'outer;
            }
            if let Some(path) = shortest_path_undirected(g, resolved[i], resolved[j], max_path_depth) {
                for id in path {
                    covered.insert(id);
                    if covered.len() >= max_subgraph_size {
                        break 'outer;
                    }
                }
            }
        }
    }
    let snapshot: Vec<NodeId> = covered.iter().copied().collect();
    for id in snapshot {
        if covered.len() >= max_subgraph_size {
            break;
        }
        if let Some(ix) = g.lookup(id) {
            for nb_ix in g.graph.neighbors_directed(ix, Direction::Outgoing) {
                if let Some(&nb_id) = g.graph.node_weight(nb_ix) {
                    covered.insert(nb_id);
                    if covered.len() >= max_subgraph_size {
                        break;
                    }
                }
            }
        }
    }
    covered
}

#[cfg(test)]
mod tests {
    use super::*;
    use codex_call_graph_store::{EdgeSource, ParsedFile, RawCallEdge, RawFnNode, RawTarget};
    use std::path::PathBuf;
    use tempfile::tempdir;

    fn raw_node(name: &str, line: u32) -> RawFnNode {
        RawFnNode {
            simple_name: name.into(),
            line,
            is_async: false,
            is_method: false,
            loc: 1,
            fingerprint: [0u8; 16],
        }
    }

    fn raw_edge(caller: &str, callee: &str, line: u32) -> RawCallEdge {
        RawCallEdge {
            caller_simple_name: caller.into(),
            caller_line: 0,
            line,
            source: EdgeSource::SimpleName,
            confidence: 0.85,
            target: RawTarget::Simple {
                callee_simple_name: callee.into(),
            },
        }
    }

    fn build_chain_pgs() -> (tempfile::TempDir, GraphStore) {
        let dir = tempdir().expect("tempdir");
        let store = GraphStore::open(dir.path()).expect("open");
        store
            .upsert_file(&ParsedFile {
                file: PathBuf::from("src/lib.rs"),
                nodes: vec![raw_node("a", 1), raw_node("b", 5), raw_node("c", 9)],
                edges: vec![raw_edge("a", "b", 2), raw_edge("b", "c", 6)],
            })
            .expect("upsert");
        (dir, store)
    }

    #[test]
    fn loaded_graph_resolves_simple_targets_to_node_ids() {
        let (_dir, store) = build_chain_pgs();
        let g = LoadedGraph::from_pgs(&store).expect("load");
        assert_eq!(g.node_count(), 3);
        assert_eq!(g.edge_count(), 2);
    }

    #[test]
    fn topo_sort_orders_dag_correctly() {
        let (_dir, store) = build_chain_pgs();
        let g = LoadedGraph::from_pgs(&store).expect("load");
        let order = topo_sort(&g).expect("topo");
        assert_eq!(order.len(), 3);
        let by_id: HashMap<NodeId, usize> = order
            .iter()
            .enumerate()
            .map(|(i, id)| (*id, i))
            .collect();
        let a_id = store.nodes_by_name("a").expect("by_name")[0].id;
        let c_id = store.nodes_by_name("c").expect("by_name")[0].id;
        assert!(by_id[&a_id] < by_id[&c_id], "a must come before c");
    }

    #[test]
    fn topo_sort_rejects_cycle() {
        let dir = tempdir().expect("tempdir");
        let store = GraphStore::open(dir.path()).expect("open");
        store
            .upsert_file(&ParsedFile {
                file: PathBuf::from("src/lib.rs"),
                nodes: vec![raw_node("x", 1), raw_node("y", 5)],
                edges: vec![raw_edge("x", "y", 2), raw_edge("y", "x", 6)],
            })
            .expect("upsert");
        let g = LoadedGraph::from_pgs(&store).expect("load");
        assert!(matches!(topo_sort(&g), Err(GraphAlgoError::CycleDetected)));
    }

    #[test]
    fn scc_components_groups_cycle_into_one_component() {
        let dir = tempdir().expect("tempdir");
        let store = GraphStore::open(dir.path()).expect("open");
        store
            .upsert_file(&ParsedFile {
                file: PathBuf::from("src/lib.rs"),
                nodes: vec![raw_node("p", 1), raw_node("q", 5), raw_node("r", 9)],
                edges: vec![
                    raw_edge("p", "q", 2),
                    raw_edge("q", "r", 6),
                    raw_edge("r", "p", 10),
                ],
            })
            .expect("upsert");
        let g = LoadedGraph::from_pgs(&store).expect("load");
        let sccs = scc_components(&g);
        let big = sccs.iter().find(|c| c.len() == 3).expect("3-cycle scc");
        let _ = big;
    }

    #[test]
    fn pagerank_normalises_to_population_size() {
        let (_dir, store) = build_chain_pgs();
        let g = LoadedGraph::from_pgs(&store).expect("load");
        let ranks = pagerank(&g, 0.85, 25);
        let sum: f32 = ranks.values().copied().sum();
        assert!((sum - 1.0).abs() < 0.05, "ranks sum drift: {sum}");
    }

    #[test]
    fn shortest_path_finds_three_step_chain() {
        let (_dir, store) = build_chain_pgs();
        let g = LoadedGraph::from_pgs(&store).expect("load");
        let a = store.nodes_by_name("a").expect("by_name")[0].id;
        let c = store.nodes_by_name("c").expect("by_name")[0].id;
        let path = shortest_path(&g, a, c).expect("path");
        assert_eq!(path.len(), 3);
    }

    #[test]
    fn steiner_subgraph_covers_seeds_and_path() {
        let (_dir, store) = build_chain_pgs();
        let g = LoadedGraph::from_pgs(&store).expect("load");
        let a = store.nodes_by_name("a").expect("by_name")[0].id;
        let c = store.nodes_by_name("c").expect("by_name")[0].id;
        let sub = steiner_subgraph(&g, &[a, c], 4, 100);
        assert!(sub.contains(&a));
        assert!(sub.contains(&c));
        let b = store.nodes_by_name("b").expect("by_name")[0].id;
        assert!(sub.contains(&b), "intermediate node b should be on the BFS path");
    }

    #[test]
    fn from_pgs_bounded_only_loads_seed_neighborhood() {
        let (_dir, store) = build_chain_pgs();
        let a = store.nodes_by_name("a").expect("by_name")[0].id;
        let g = LoadedGraph::from_pgs_bounded(&store, &[a], 1, 100).expect("bounded");
        assert!(g.lookup(a).is_some(), "seed must be present");
        let b = store.nodes_by_name("b").expect("by_name")[0].id;
        assert!(g.lookup(b).is_some(), "1-hop neighbor must be present");
        let c = store.nodes_by_name("c").expect("by_name")[0].id;
        assert!(g.lookup(c).is_none(), "2-hops-away node must NOT be present at radius=1");
    }

    #[test]
    fn from_pgs_bounded_respects_max_nodes_cap() {
        let (_dir, store) = build_chain_pgs();
        let a = store.nodes_by_name("a").expect("by_name")[0].id;
        let g = LoadedGraph::from_pgs_bounded(&store, &[a], 99, 2).expect("bounded");
        assert!(
            g.node_count() <= 2,
            "max_nodes cap must be honored, got {} nodes",
            g.node_count()
        );
        assert!(g.lookup(a).is_some(), "seed must be retained even when capped");
    }

    #[test]
    fn from_pgs_bounded_includes_simple_name_callers() {
        let (_dir, store) = build_chain_pgs();
        let b = store.nodes_by_name("b").expect("by_name")[0].id;
        let g = LoadedGraph::from_pgs_bounded(&store, &[b], 1, 100).expect("bounded");
        let a = store.nodes_by_name("a").expect("by_name")[0].id;
        assert!(
            g.lookup(a).is_some(),
            "a calls b via simple-name; bounded BFS must walk the inverse index"
        );
    }
}
