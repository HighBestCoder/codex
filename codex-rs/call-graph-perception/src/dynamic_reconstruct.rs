use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DynEvent {
    Enter {
        span_id: u64,
        parent_span_id: Option<u64>,
        fn_name: String,
        target: String,
        ts_ns: u64,
        thread: String,
    },
    Exit {
        span_id: u64,
        ts_ns: u64,
    },
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CallTreeNode {
    pub span_id: u64,
    pub fn_name: String,
    pub target: String,
    pub thread: String,
    pub duration_ns: u64,
    pub children: Vec<CallTreeNode>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DynEdgeRecord {
    pub caller: String,
    pub callee: String,
    pub hit_count: u64,
    pub total_latency_ns: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ReconstructResult {
    pub trees: Vec<CallTreeNode>,
    pub edges: Vec<DynEdgeRecord>,
    pub event_count: usize,
}

#[derive(Debug, thiserror::Error)]
pub enum ReconstructError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("json parse error: {0}")]
    Json(#[from] serde_json::Error),
}

pub fn reconstruct_from_events(events: &[DynEvent]) -> ReconstructResult {
    let mut open: BTreeMap<u64, CallTreeNode> = BTreeMap::new();
    let mut parents: BTreeMap<u64, Option<u64>> = BTreeMap::new();
    let mut start_ts: BTreeMap<u64, u64> = BTreeMap::new();
    let mut roots: Vec<CallTreeNode> = Vec::new();
    let mut edge_acc: BTreeMap<(String, String), (u64, u64)> = BTreeMap::new();

    for ev in events {
        match ev {
            DynEvent::Enter {
                span_id,
                parent_span_id,
                fn_name,
                target,
                ts_ns,
                thread,
            } => {
                open.insert(
                    *span_id,
                    CallTreeNode {
                        span_id: *span_id,
                        fn_name: fn_name.clone(),
                        target: target.clone(),
                        thread: thread.clone(),
                        duration_ns: 0,
                        children: Vec::new(),
                    },
                );
                parents.insert(*span_id, *parent_span_id);
                start_ts.insert(*span_id, *ts_ns);
                if let Some(parent_span_id) = parent_span_id {
                    if let Some(parent) = open.get(parent_span_id) {
                        let key = (parent.fn_name.clone(), fn_name.clone());
                        let entry = edge_acc.entry(key).or_insert((0, 0));
                        entry.0 += 1;
                    }
                }
            }
            DynEvent::Exit { span_id, ts_ns } => {
                let Some(mut node) = open.remove(span_id) else {
                    continue;
                };
                if let Some(start) = start_ts.remove(span_id) {
                    node.duration_ns = ts_ns.saturating_sub(start);
                }
                let parent = parents.remove(span_id).unwrap_or(None);
                if let Some(parent_id) = parent {
                    let key_lookup = open
                        .get(&parent_id)
                        .map(|p| (p.fn_name.clone(), node.fn_name.clone()));
                    if let Some(key) = key_lookup {
                        let entry = edge_acc.entry(key).or_insert((0, 0));
                        entry.1 += node.duration_ns;
                    }
                    if let Some(parent_node) = open.get_mut(&parent_id) {
                        parent_node.children.push(node);
                        continue;
                    }
                }
                roots.push(node);
            }
        }
    }
    for (_id, node) in open {
        roots.push(node);
    }

    let edges: Vec<DynEdgeRecord> = edge_acc
        .into_iter()
        .map(|((caller, callee), (hits, total_latency))| DynEdgeRecord {
            caller,
            callee,
            hit_count: hits,
            total_latency_ns: total_latency,
        })
        .collect();

    ReconstructResult {
        trees: roots,
        edges,
        event_count: events.len(),
    }
}

pub fn read_events_jsonl(path: &Path) -> Result<Vec<DynEvent>, ReconstructError> {
    let raw = std::fs::read_to_string(path)?;
    let mut events = Vec::new();
    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        events.push(serde_json::from_str(trimmed)?);
    }
    Ok(events)
}

pub fn demangle_cpp_simple(name: &str) -> String {
    if !name.starts_with("_Z") {
        return name.to_string();
    }
    match cpp_demangle::Symbol::new(name) {
        Ok(sym) => {
            let opts = cpp_demangle::DemangleOptions::new()
                .no_params()
                .no_return_type();
            let demangled = sym.demangle(&opts).unwrap_or_else(|_| name.to_string());
            demangled.rsplit("::").next().unwrap_or(&demangled).to_string()
        }
        Err(_) => name.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn enter(id: u64, parent: Option<u64>, name: &str, ts: u64) -> DynEvent {
        DynEvent::Enter {
            span_id: id,
            parent_span_id: parent,
            fn_name: name.into(),
            target: format!("crate::{name}"),
            ts_ns: ts,
            thread: "main".into(),
        }
    }
    fn exit(id: u64, ts: u64) -> DynEvent {
        DynEvent::Exit { span_id: id, ts_ns: ts }
    }

    #[test]
    fn reconstructs_three_step_chain() {
        let events = vec![
            enter(1, None, "run_workload", 100),
            enter(2, Some(1), "quadruple", 110),
            enter(3, Some(2), "double", 120),
            enter(4, Some(3), "add", 130),
            exit(4, 135),
            exit(3, 140),
            enter(5, Some(2), "double", 145),
            enter(6, Some(5), "add", 150),
            exit(6, 155),
            exit(5, 160),
            exit(2, 170),
            exit(1, 200),
        ];
        let result = reconstruct_from_events(&events);
        assert_eq!(result.event_count, 12);
        assert_eq!(result.trees.len(), 1);
        let edges: BTreeMap<_, _> = result
            .edges
            .iter()
            .map(|e| ((e.caller.as_str(), e.callee.as_str()), e.hit_count))
            .collect();
        assert_eq!(edges.get(&("quadruple", "double")), Some(&2));
        assert_eq!(edges.get(&("double", "add")), Some(&2));
        assert_eq!(edges.get(&("run_workload", "quadruple")), Some(&1));
    }

    #[test]
    fn handles_dangling_exit_events_gracefully() {
        let events = vec![exit(99, 0)];
        let result = reconstruct_from_events(&events);
        assert_eq!(result.trees.len(), 0);
        assert!(result.edges.is_empty());
    }

    #[test]
    fn demangles_common_itanium_symbols_to_simple_name() {
        assert_eq!(demangle_cpp_simple("_Z9quadruplei"), "quadruple");
        assert_eq!(demangle_cpp_simple("_Z9double_iti"), "double_it");
        assert_eq!(demangle_cpp_simple("_Z3addii"), "add");
        assert_eq!(demangle_cpp_simple("main"), "main");
        assert_eq!(demangle_cpp_simple("not_mangled"), "not_mangled");
    }
}
