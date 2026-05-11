use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use sled::{Db, Tree};

use crate::error::CallGraphStoreError;
use crate::id_dict::{decode_u32, decode_u32_pair, encode_u32, encode_u32_pair, StringDict};
use crate::schema::{
    CallEdge, EdgeTarget, FnNode, NodeId, NodeMetrics, ParsedFile, RawTarget, ResolvedTarget,
    StoredEdge, StoredFnNode, StringId,
};

const TREE_NODES: &str = "nodes";
const TREE_EDGES_OUT: &str = "edges_out";
const TREE_EDGES_IN: &str = "edges_in";
const TREE_BY_FILE: &str = "by_file";
const TREE_BY_NAME: &str = "by_name";
const TREE_COUNTERS: &str = "counters";

const COUNTER_NEXT_NODE: &[u8] = b"__next_node_id";
const COUNTER_NEXT_EDGE: &[u8] = b"__next_edge_counter";

#[derive(Debug, Clone, Default)]
pub struct UpsertStats {
    pub removed_nodes: usize,
    pub added_nodes: usize,
    pub removed_edges: usize,
    pub added_edges: usize,
}

#[derive(Debug, Clone, Default)]
pub struct StoreStats {
    pub node_count: usize,
    pub out_edge_count: usize,
    pub in_edge_count: usize,
    pub interned_string_count: usize,
}

pub struct GraphStore {
    db: Db,
    dict: StringDict,
    nodes: Tree,
    edges_out: Tree,
    edges_in: Tree,
    by_file: Tree,
    by_name: Tree,
    counters: Tree,
    next_node_id: AtomicU32,
    next_edge_counter: AtomicU64,
    inner_arc: Arc<()>,
}

impl GraphStore {
    pub fn open(path: &Path) -> Result<Self, CallGraphStoreError> {
        let db = sled::Config::default()
            .path(path)
            .cache_capacity(64 * 1024 * 1024)
            .flush_every_ms(Some(1000))
            .open()?;
        Self::from_db(db)
    }

    /// Try to open the PGS, retrying briefly if another process holds the
    /// sled file lock. Falls through to the same error type as `open` after
    /// the retry window is exhausted, so callers can decide whether to
    /// degrade gracefully (e.g. the graph decorator) or surface the error.
    pub fn open_with_retry(
        path: &Path,
        attempts: u32,
        delay_ms: u64,
    ) -> Result<Self, CallGraphStoreError> {
        let mut last_err: Option<CallGraphStoreError> = None;
        for _ in 0..attempts.max(1) {
            match Self::open(path) {
                Ok(store) => return Ok(store),
                Err(err) => {
                    if !is_sled_lock_contention(&err) {
                        return Err(err);
                    }
                    last_err = Some(err);
                    std::thread::sleep(std::time::Duration::from_millis(delay_ms));
                }
            }
        }
        Err(last_err.expect("at least one attempt"))
    }

    fn from_db(db: sled::Db) -> Result<Self, CallGraphStoreError> {
        let counters = db.open_tree(TREE_COUNTERS)?;
        let dict = StringDict::open(&db, counters.clone())?;
        let nodes = db.open_tree(TREE_NODES)?;
        let edges_out = db.open_tree(TREE_EDGES_OUT)?;
        let edges_in = db.open_tree(TREE_EDGES_IN)?;
        let by_file = db.open_tree(TREE_BY_FILE)?;
        let by_name = db.open_tree(TREE_BY_NAME)?;

        let next_node_id = counters
            .get(COUNTER_NEXT_NODE)?
            .map(|raw| decode_u32(&raw))
            .unwrap_or(1);
        let next_edge_counter = counters
            .get(COUNTER_NEXT_EDGE)?
            .map(|raw| {
                let mut buf = [0u8; 8];
                buf.copy_from_slice(&raw[..8]);
                u64::from_le_bytes(buf)
            })
            .unwrap_or(1);

        Ok(Self {
            db,
            dict,
            nodes,
            edges_out,
            edges_in,
            by_file,
            by_name,
            counters,
            next_node_id: AtomicU32::new(next_node_id),
            next_edge_counter: AtomicU64::new(next_edge_counter),
            inner_arc: Arc::new(()),
        })
    }

    pub fn intern(&self, s: &str) -> Result<StringId, CallGraphStoreError> {
        self.dict.intern(s)
    }

    pub fn lookup_string(&self, id: StringId) -> Result<Option<String>, CallGraphStoreError> {
        self.dict.lookup(id)
    }

    pub fn flush(&self) -> Result<(), CallGraphStoreError> {
        self.persist_counters()?;
        self.db.flush()?;
        Ok(())
    }

    pub fn size_on_disk(&self) -> Result<u64, CallGraphStoreError> {
        Ok(self.db.size_on_disk()?)
    }

    pub fn stats(&self) -> Result<StoreStats, CallGraphStoreError> {
        Ok(StoreStats {
            node_count: self.nodes.len(),
            out_edge_count: self.edges_out.len(),
            in_edge_count: self.edges_in.len(),
            interned_string_count: self.dict.len(),
        })
    }

    pub fn node(&self, id: NodeId) -> Result<Option<FnNode>, CallGraphStoreError> {
        let key = encode_u32(id);
        let Some(raw) = self.nodes.get(key)? else {
            return Ok(None);
        };
        let stored: StoredFnNode = postcard::from_bytes(&raw)?;
        self.hydrate_node(stored).map(Some)
    }

    pub fn nodes_by_name(&self, name: &str) -> Result<Vec<FnNode>, CallGraphStoreError> {
        let Some(name_id) = self.dict.lookup_id(name)? else {
            return Ok(Vec::new());
        };
        let prefix = encode_u32(name_id);
        let mut out = Vec::new();
        for kv in self.by_name.scan_prefix(prefix) {
            let (k, _) = kv?;
            let (_, node_id) = decode_u32_pair(&k);
            if let Some(node) = self.node(node_id)? {
                out.push(node);
            }
        }
        Ok(out)
    }

    pub fn nodes_by_file(&self, file: &Path) -> Result<Vec<FnNode>, CallGraphStoreError> {
        let Some(file_id) = self.dict.lookup_id(&file.display().to_string())? else {
            return Ok(Vec::new());
        };
        let prefix = encode_u32(file_id);
        let mut out = Vec::new();
        for kv in self.by_file.scan_prefix(prefix) {
            let (k, _) = kv?;
            let (_, node_id) = decode_u32_pair(&k);
            if let Some(node) = self.node(node_id)? {
                out.push(node);
            }
        }
        Ok(out)
    }

    pub fn out_edges(&self, caller: NodeId) -> Result<Vec<CallEdge>, CallGraphStoreError> {
        self.edges_for_node(&self.edges_out, caller)
    }

    pub fn in_edges(&self, callee: NodeId) -> Result<Vec<CallEdge>, CallGraphStoreError> {
        self.edges_for_node(&self.edges_in, callee)
    }

    pub fn iter_nodes(&self) -> impl Iterator<Item = Result<FnNode, CallGraphStoreError>> + '_ {
        self.nodes.iter().map(|kv| {
            let (_, raw) = kv?;
            let stored: StoredFnNode = postcard::from_bytes(&raw)?;
            self.hydrate_node(stored)
        })
    }

    pub fn iter_out_edges(&self) -> impl Iterator<Item = Result<CallEdge, CallGraphStoreError>> + '_ {
        self.edges_out.iter().map(|kv| {
            let (_, raw) = kv?;
            let stored: StoredEdge = postcard::from_bytes(&raw)?;
            self.hydrate_edge(stored)
        })
    }

    pub fn upsert_file(&self, parsed: &ParsedFile) -> Result<UpsertStats, CallGraphStoreError> {
        let mut stats = UpsertStats::default();
        let removed = self.remove_file(&parsed.file)?;
        stats.removed_nodes = removed.removed_nodes;
        stats.removed_edges = removed.removed_edges;

        let file_path_str = parsed.file.display().to_string();
        let file_id = self.dict.intern(&file_path_str)?;

        let mut node_id_for_simple_name: std::collections::HashMap<String, Vec<NodeId>> =
            std::collections::HashMap::new();

        let mut node_batch = sled::Batch::default();
        let mut by_file_batch = sled::Batch::default();
        let mut by_name_batch = sled::Batch::default();

        for raw in &parsed.nodes {
            let id = self.allocate_node_id()?;
            let name_id = self.dict.intern(&raw.simple_name)?;
            let stored = StoredFnNode {
                id,
                simple_name: name_id,
                file: file_id,
                line: raw.line,
                is_async: raw.is_async,
                is_method: raw.is_method,
                loc: raw.loc,
                fingerprint: raw.fingerprint,
                semantic_brief: None,
                metrics: NodeMetrics::default(),
            };
            let value = postcard::to_allocvec(&stored)?;
            node_batch.insert(&encode_u32(id)[..], value);
            by_file_batch.insert(&encode_u32_pair(file_id, id)[..], &[][..]);
            by_name_batch.insert(&encode_u32_pair(name_id, id)[..], &[][..]);
            node_id_for_simple_name
                .entry(raw.simple_name.clone())
                .or_default()
                .push(id);
            stats.added_nodes += 1;
        }
        self.nodes.apply_batch(node_batch)?;
        self.by_file.apply_batch(by_file_batch)?;
        self.by_name.apply_batch(by_name_batch)?;

        let mut out_batch = sled::Batch::default();
        let mut in_batch = sled::Batch::default();

        for raw_edge in &parsed.edges {
            let Some(caller_ids) = node_id_for_simple_name.get(&raw_edge.caller_simple_name)
            else {
                continue;
            };

            for &caller_id in caller_ids {
                let target = match &raw_edge.target {
                    RawTarget::Simple { callee_simple_name } => {
                        let callee_name = self.dict.intern(callee_simple_name)?;
                        EdgeTarget::Simple { callee_name }
                    }
                    RawTarget::Resolved { callee_id } => EdgeTarget::Resolved {
                        callee_id: *callee_id,
                    },
                };
                let stored = StoredEdge {
                    caller_id,
                    line: raw_edge.line,
                    source: raw_edge.source,
                    confidence: raw_edge.confidence,
                    target: target.clone(),
                };
                let counter = self.allocate_edge_counter()?;
                let value = postcard::to_allocvec(&stored)?;
                let out_key = edge_key(caller_id, counter);
                out_batch.insert(&out_key[..], value.clone());
                if let EdgeTarget::Resolved { callee_id } = target {
                    let in_key = edge_key(callee_id, counter);
                    in_batch.insert(&in_key[..], value);
                }
                stats.added_edges += 1;
            }
        }
        self.edges_out.apply_batch(out_batch)?;
        self.edges_in.apply_batch(in_batch)?;
        self.persist_counters()?;
        Ok(stats)
    }

    pub fn remove_file(&self, file: &Path) -> Result<UpsertStats, CallGraphStoreError> {
        let mut stats = UpsertStats::default();
        let file_path_str = file.display().to_string();
        let Some(file_id) = self.dict.lookup_id(&file_path_str)? else {
            return Ok(stats);
        };
        let prefix = encode_u32(file_id);
        let mut victims: Vec<NodeId> = Vec::new();
        for kv in self.by_file.scan_prefix(prefix) {
            let (k, _) = kv?;
            let (_, node_id) = decode_u32_pair(&k);
            victims.push(node_id);
        }
        if victims.is_empty() {
            return Ok(stats);
        }

        let mut name_keys_to_drop: Vec<Vec<u8>> = Vec::new();
        for &node_id in &victims {
            if let Some(stored_raw) = self.nodes.get(encode_u32(node_id))? {
                let stored: StoredFnNode = postcard::from_bytes(&stored_raw)?;
                name_keys_to_drop.push(encode_u32_pair(stored.simple_name, stored.id).to_vec());
            }
        }

        let mut by_file_batch = sled::Batch::default();
        let mut by_name_batch = sled::Batch::default();
        let mut nodes_batch = sled::Batch::default();
        for &node_id in &victims {
            by_file_batch.remove(&encode_u32_pair(file_id, node_id)[..]);
            nodes_batch.remove(&encode_u32(node_id)[..]);
            stats.removed_nodes += 1;
        }
        for k in &name_keys_to_drop {
            by_name_batch.remove(&k[..]);
        }
        self.by_file.apply_batch(by_file_batch)?;
        self.by_name.apply_batch(by_name_batch)?;
        self.nodes.apply_batch(nodes_batch)?;

        for &node_id in &victims {
            stats.removed_edges += self.remove_edges_for_caller(node_id)?;
        }
        Ok(stats)
    }

    pub fn clear(&self) -> Result<(), CallGraphStoreError> {
        self.nodes.clear()?;
        self.edges_out.clear()?;
        self.edges_in.clear()?;
        self.by_file.clear()?;
        self.by_name.clear()?;
        self.next_node_id.store(1, Ordering::SeqCst);
        self.next_edge_counter.store(1, Ordering::SeqCst);
        self.persist_counters()?;
        Ok(())
    }

    fn allocate_node_id(&self) -> Result<NodeId, CallGraphStoreError> {
        let id = self.next_node_id.fetch_add(1, Ordering::SeqCst);
        if id == 0 {
            return Err(CallGraphStoreError::IdExhausted { what: "node id" });
        }
        Ok(id)
    }

    fn allocate_edge_counter(&self) -> Result<u64, CallGraphStoreError> {
        let n = self.next_edge_counter.fetch_add(1, Ordering::SeqCst);
        if n == 0 {
            return Err(CallGraphStoreError::IdExhausted { what: "edge counter" });
        }
        Ok(n)
    }

    fn persist_counters(&self) -> Result<(), CallGraphStoreError> {
        let next_node = self.next_node_id.load(Ordering::SeqCst);
        let next_edge = self.next_edge_counter.load(Ordering::SeqCst);
        self.counters
            .insert(COUNTER_NEXT_NODE, &next_node.to_le_bytes()[..])?;
        self.counters
            .insert(COUNTER_NEXT_EDGE, &next_edge.to_le_bytes()[..])?;
        Ok(())
    }

    fn edges_for_node(&self, tree: &Tree, node: NodeId) -> Result<Vec<CallEdge>, CallGraphStoreError> {
        let prefix = encode_u32(node);
        let mut out = Vec::new();
        for kv in tree.scan_prefix(prefix) {
            let (_, raw) = kv?;
            let stored: StoredEdge = postcard::from_bytes(&raw)?;
            out.push(self.hydrate_edge(stored)?);
        }
        Ok(out)
    }

    fn remove_edges_for_caller(&self, caller_id: NodeId) -> Result<usize, CallGraphStoreError> {
        let prefix = encode_u32(caller_id);
        let mut victim_keys: Vec<Vec<u8>> = Vec::new();
        let mut in_keys_to_drop: Vec<Vec<u8>> = Vec::new();
        for kv in self.edges_out.scan_prefix(prefix) {
            let (k, raw) = kv?;
            victim_keys.push(k.to_vec());
            let stored: StoredEdge = postcard::from_bytes(&raw)?;
            if let EdgeTarget::Resolved { callee_id } = stored.target {
                let mut in_key = encode_u32(callee_id).to_vec();
                in_key.extend_from_slice(&k[4..]);
                in_keys_to_drop.push(in_key);
            }
        }
        let count = victim_keys.len();
        let mut out_batch = sled::Batch::default();
        for k in victim_keys {
            out_batch.remove(&k[..]);
        }
        self.edges_out.apply_batch(out_batch)?;

        let mut in_batch = sled::Batch::default();
        for k in in_keys_to_drop {
            in_batch.remove(&k[..]);
        }
        self.edges_in.apply_batch(in_batch)?;
        Ok(count)
    }

    fn hydrate_node(&self, stored: StoredFnNode) -> Result<FnNode, CallGraphStoreError> {
        let simple_name = self.dict.lookup_required(stored.simple_name)?;
        let file_str = self.dict.lookup_required(stored.file)?;
        let semantic_brief = match stored.semantic_brief {
            Some(id) => self.dict.lookup(id)?,
            None => None,
        };
        Ok(FnNode {
            id: stored.id,
            simple_name,
            file: PathBuf::from(file_str),
            line: stored.line,
            is_async: stored.is_async,
            is_method: stored.is_method,
            loc: stored.loc,
            fingerprint: stored.fingerprint,
            semantic_brief,
            metrics: stored.metrics,
        })
    }

    fn hydrate_edge(&self, stored: StoredEdge) -> Result<CallEdge, CallGraphStoreError> {
        let target = match stored.target {
            EdgeTarget::Simple { callee_name } => ResolvedTarget::Simple {
                callee_name: self.dict.lookup_required(callee_name)?,
            },
            EdgeTarget::Resolved { callee_id } => ResolvedTarget::Resolved { callee_id },
        };
        Ok(CallEdge {
            caller_id: stored.caller_id,
            line: stored.line,
            source: stored.source,
            confidence: stored.confidence,
            target,
        })
    }
}

fn edge_key(node_id: NodeId, counter: u64) -> [u8; 12] {
    let mut buf = [0u8; 12];
    buf[..4].copy_from_slice(&node_id.to_le_bytes());
    buf[4..].copy_from_slice(&counter.to_le_bytes());
    buf
}

impl Clone for GraphStore {
    fn clone(&self) -> Self {
        Self {
            db: self.db.clone(),
            dict: StringDict::open(&self.db, self.counters.clone())
                .expect("dict trees already open"),
            nodes: self.nodes.clone(),
            edges_out: self.edges_out.clone(),
            edges_in: self.edges_in.clone(),
            by_file: self.by_file.clone(),
            by_name: self.by_name.clone(),
            counters: self.counters.clone(),
            next_node_id: AtomicU32::new(self.next_node_id.load(Ordering::SeqCst)),
            next_edge_counter: AtomicU64::new(self.next_edge_counter.load(Ordering::SeqCst)),
            inner_arc: self.inner_arc.clone(),
        }
    }
}

fn is_sled_lock_contention(err: &CallGraphStoreError) -> bool {
    let msg = err.to_string().to_lowercase();
    msg.contains("could not acquire lock") || msg.contains("wouldblock") || msg.contains("resource temporarily unavailable")
}
