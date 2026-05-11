mod error;
mod id_dict;
mod schema;
mod store;

pub use error::CallGraphStoreError;
pub use schema::{
    CallEdge, EdgeSource, EdgeTarget, FnNode, NodeId, NodeMetrics, ParsedFile, RawCallEdge,
    RawFnNode, RawTarget, ResolvedTarget, StoredEdge, StoredFnNode, StringId,
};
pub use store::{GraphStore, StoreStats, UpsertStats};

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tempfile::tempdir;

    fn raw_node(name: &str, line: u32) -> RawFnNode {
        RawFnNode {
            simple_name: name.to_string(),
            line,
            is_async: false,
            is_method: false,
            loc: 1,
            fingerprint: blake3::hash(name.as_bytes()).as_bytes()[..16]
                .try_into()
                .expect("16 bytes"),
        }
    }

    fn raw_edge(caller: &str, callee: &str, line: u32) -> RawCallEdge {
        RawCallEdge {
            caller_simple_name: caller.to_string(),
            caller_line: 0,
            line,
            source: EdgeSource::SimpleName,
            confidence: EdgeSource::SimpleName.default_confidence(),
            target: RawTarget::Simple {
                callee_simple_name: callee.to_string(),
            },
        }
    }

    #[test]
    fn open_persist_reopen_round_trips_string_dictionary() {
        let dir = tempdir().expect("tempdir");
        {
            let store = GraphStore::open(dir.path()).expect("open");
            let id_a = store.intern("foo").expect("intern");
            let id_b = store.intern("bar").expect("intern");
            let id_a_again = store.intern("foo").expect("intern");
            assert_eq!(id_a, id_a_again);
            assert_ne!(id_a, id_b);
            assert_eq!(store.lookup_string(id_a).expect("lookup"), Some("foo".into()));
            store.flush().expect("flush");
        }
        let store = GraphStore::open(dir.path()).expect("reopen");
        let id_a = store.intern("foo").expect("intern");
        assert_eq!(store.lookup_string(id_a).expect("lookup"), Some("foo".into()));
    }

    #[test]
    fn upsert_file_persists_nodes_and_simple_edges() {
        let dir = tempdir().expect("tempdir");
        let store = GraphStore::open(dir.path()).expect("open");
        let parsed = ParsedFile {
            file: PathBuf::from("src/foo.rs"),
            nodes: vec![raw_node("add", 1), raw_node("double", 5), raw_node("quad", 10)],
            edges: vec![raw_edge("double", "add", 6), raw_edge("quad", "double", 11)],
        };
        let stats = store.upsert_file(&parsed).expect("upsert");
        assert_eq!(stats.added_nodes, 3);
        assert_eq!(stats.added_edges, 2);

        let by_name = store.nodes_by_name("double").expect("by_name");
        assert_eq!(by_name.len(), 1);
        assert_eq!(by_name[0].simple_name, "double");
        assert_eq!(by_name[0].file, PathBuf::from("src/foo.rs"));

        let by_file = store.nodes_by_file(&PathBuf::from("src/foo.rs")).expect("by_file");
        assert_eq!(by_file.len(), 3);

        let double_id = by_name[0].id;
        let out = store.out_edges(double_id).expect("out_edges");
        assert_eq!(out.len(), 1);
        match &out[0].target {
            ResolvedTarget::Simple { callee_name } => assert_eq!(callee_name, "add"),
            _ => panic!("expected simple-name edge"),
        }
    }

    #[test]
    fn remove_file_drops_nodes_and_outgoing_edges() {
        let dir = tempdir().expect("tempdir");
        let store = GraphStore::open(dir.path()).expect("open");
        let parsed = ParsedFile {
            file: PathBuf::from("src/foo.rs"),
            nodes: vec![raw_node("a", 1), raw_node("b", 5)],
            edges: vec![raw_edge("a", "b", 2)],
        };
        store.upsert_file(&parsed).expect("upsert");
        let removed = store
            .remove_file(&PathBuf::from("src/foo.rs"))
            .expect("remove");
        assert_eq!(removed.removed_nodes, 2);
        assert_eq!(removed.removed_edges, 1);
        assert!(store
            .nodes_by_file(&PathBuf::from("src/foo.rs"))
            .expect("by_file")
            .is_empty());
        assert!(store.nodes_by_name("a").expect("by_name").is_empty());
    }

    #[test]
    fn upsert_same_file_replaces_old_state() {
        let dir = tempdir().expect("tempdir");
        let store = GraphStore::open(dir.path()).expect("open");
        let v1 = ParsedFile {
            file: PathBuf::from("src/foo.rs"),
            nodes: vec![raw_node("a", 1), raw_node("b", 5)],
            edges: vec![raw_edge("a", "b", 2)],
        };
        store.upsert_file(&v1).expect("upsert v1");

        let v2 = ParsedFile {
            file: PathBuf::from("src/foo.rs"),
            nodes: vec![raw_node("a", 2), raw_node("c", 6)],
            edges: vec![raw_edge("a", "c", 3)],
        };
        let stats = store.upsert_file(&v2).expect("upsert v2");
        assert_eq!(stats.removed_nodes, 2);
        assert_eq!(stats.added_nodes, 2);
        assert!(store.nodes_by_name("b").expect("by_name").is_empty());
        let c_nodes = store.nodes_by_name("c").expect("by_name");
        assert_eq!(c_nodes.len(), 1);
    }

    #[test]
    fn iter_nodes_yields_all_persisted_nodes() {
        let dir = tempdir().expect("tempdir");
        let store = GraphStore::open(dir.path()).expect("open");
        store
            .upsert_file(&ParsedFile {
                file: PathBuf::from("a.rs"),
                nodes: vec![raw_node("x", 1), raw_node("y", 2)],
                edges: vec![],
            })
            .expect("upsert a");
        store
            .upsert_file(&ParsedFile {
                file: PathBuf::from("b.rs"),
                nodes: vec![raw_node("z", 1)],
                edges: vec![],
            })
            .expect("upsert b");

        let names: Vec<String> = store
            .iter_nodes()
            .map(|n| n.expect("node").simple_name)
            .collect();
        assert_eq!(names.len(), 3);
        let mut sorted = names.clone();
        sorted.sort();
        assert_eq!(sorted, vec!["x", "y", "z"]);
    }

    #[test]
    fn resolved_edges_populate_in_edge_index() {
        let dir = tempdir().expect("tempdir");
        let store = GraphStore::open(dir.path()).expect("open");
        store
            .upsert_file(&ParsedFile {
                file: PathBuf::from("src/foo.rs"),
                nodes: vec![raw_node("caller", 1), raw_node("callee", 5)],
                edges: vec![],
            })
            .expect("upsert");
        let caller_id = store.nodes_by_name("caller").expect("by_name")[0].id;
        let callee_id = store.nodes_by_name("callee").expect("by_name")[0].id;
        store
            .upsert_file(&ParsedFile {
                file: PathBuf::from("src/bar.rs"),
                nodes: vec![raw_node("driver", 1)],
                edges: vec![RawCallEdge {
                    caller_simple_name: "driver".into(),
                    caller_line: 0,
                    line: 4,
                    source: EdgeSource::ScipInternal,
                    confidence: 0.99,
                    target: RawTarget::Resolved {
                        callee_id: callee_id,
                    },
                }],
            })
            .expect("upsert bar");
        let driver_id = store.nodes_by_name("driver").expect("by_name")[0].id;
        let out = store.out_edges(driver_id).expect("out");
        assert_eq!(out.len(), 1);
        match out[0].target {
            ResolvedTarget::Resolved { callee_id: id } => assert_eq!(id, callee_id),
            _ => panic!("expected resolved edge"),
        }
        let in_edges = store.in_edges(callee_id).expect("in");
        assert_eq!(in_edges.len(), 1);
        let _ = caller_id;
    }

    #[test]
    fn open_with_retry_surfaces_non_lock_errors_immediately() {
        let path = std::path::Path::new("/nonexistent/codex-cgs/should-not-exist");
        let started = std::time::Instant::now();
        let result = GraphStore::open_with_retry(path, 5, 200);
        assert!(result.is_err(), "should fail on missing path");
        assert!(
            started.elapsed() < std::time::Duration::from_millis(500),
            "non-lock errors must not trigger the retry loop"
        );
        let err = result.err().expect("error already asserted present");
        match err {
            CallGraphStoreError::Sled(_) | CallGraphStoreError::Io(_) => {}
            other => panic!("unexpected error variant: {other:?}"),
        }
    }

    #[test]
    fn open_with_retry_eventually_acquires_lock_when_holder_releases() {
        let dir = tempdir().expect("tempdir");
        let holder = GraphStore::open(dir.path()).expect("first open holds the lock");
        let dir_path = dir.path().to_path_buf();
        let handle = std::thread::spawn(move || {
            GraphStore::open_with_retry(&dir_path, 25, 100)
        });
        std::thread::sleep(std::time::Duration::from_millis(300));
        drop(holder);
        let result = handle.join().expect("join thread");
        assert!(result.is_ok(), "retry should succeed once holder dropped");
        let store = result.unwrap_or_else(|_| unreachable!());
        store.intern("after-release").expect("usable after retry");
    }
}
