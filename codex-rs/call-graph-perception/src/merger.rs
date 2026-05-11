use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use codex_call_graph_store::{EdgeSource, ParsedFile, RawCallEdge, RawFnNode, RawTarget};

use crate::static_scip::{ScipResult, ScipFn};
use crate::static_simple::ProjectParse;

const EXTERN_FILE_LABEL: &str = "<extern>";

#[derive(Debug, Clone, Default)]
pub struct MergedProject {
    pub project_files: Vec<ParsedFile>,
    pub external_file: Option<ParsedFile>,
    pub stats: MergeStats,
}

#[derive(Debug, Clone, Default)]
pub struct MergeStats {
    pub simple_name_edges_kept: usize,
    pub simple_name_edges_dropped: usize,
    pub scip_internal_edges_added: usize,
    pub scip_external_edges_added: usize,
    pub external_node_count: usize,
    pub project_files_with_scip_edges: usize,
}

pub fn merge(
    project_root: &Path,
    mut simple: ProjectParse,
    scip: Option<ScipResult>,
) -> MergedProject {
    let Some(scip) = scip else {
        let project_files = simple.files;
        let stats = MergeStats {
            simple_name_edges_kept: project_files.iter().map(|f| f.edges.len()).sum(),
            ..MergeStats::default()
        };
        return MergedProject {
            project_files,
            external_file: None,
            stats,
        };
    };

    promote_scip_fns_to_simple_files(project_root, &mut simple, &scip);

    let scip_fn_lookup: HashMap<&str, &ScipFn> = scip
        .functions
        .iter()
        .map(|f| (f.symbol.as_str(), f))
        .collect();
    let external_lookup: HashSet<&str> = scip
        .external_callees
        .iter()
        .map(|c| c.symbol.as_str())
        .collect();

    let mut edges_by_caller_file: BTreeMap<PathBuf, Vec<PendingScipEdge>> = BTreeMap::new();
    let mut covered_pairs: HashMap<PathBuf, HashSet<(String, String)>> = HashMap::new();
    let mut stats = MergeStats::default();

    for edge in &scip.edges {
        let Some(caller_fn) = scip_fn_lookup.get(edge.caller_symbol.as_str()) else {
            continue;
        };
        let caller_abs = absolute_for(project_root, &caller_fn.file);
        let caller_simple = simple_name_from_scip_symbol(&edge.caller_symbol);

        let (callee_simple, source) = if edge.is_external {
            if !external_lookup.contains(edge.callee_symbol.as_str()) {
                continue;
            }
            (edge.callee_symbol.clone(), EdgeSource::ScipExternal)
        } else {
            let Some(callee_fn) = scip_fn_lookup.get(edge.callee_symbol.as_str()) else {
                continue;
            };
            (
                simple_name_from_scip_symbol(&callee_fn.symbol),
                EdgeSource::ScipInternal,
            )
        };

        covered_pairs
            .entry(caller_abs.clone())
            .or_default()
            .insert((caller_simple.clone(), callee_simple.clone()));

        edges_by_caller_file
            .entry(caller_abs)
            .or_default()
            .push(PendingScipEdge {
                caller_simple,
                callee_simple,
                line: edge.line,
                source,
                confidence: source.default_confidence(),
            });

        match source {
            EdgeSource::ScipInternal => stats.scip_internal_edges_added += 1,
            EdgeSource::ScipExternal => stats.scip_external_edges_added += 1,
            _ => {}
        }
    }

    let mut project_files: Vec<ParsedFile> = Vec::with_capacity(simple.files.len());
    for mut parsed in simple.files {
        let covered = covered_pairs.get(&parsed.file);
        let original_edge_count = parsed.edges.len();
        if let Some(covered) = covered {
            parsed.edges.retain(|edge| {
                let callee = match &edge.target {
                    RawTarget::Simple { callee_simple_name } => callee_simple_name.clone(),
                    RawTarget::Resolved { .. } => return true,
                };
                let key = (edge.caller_simple_name.clone(), callee);
                let dropped = covered.contains(&key);
                if dropped {
                    stats.simple_name_edges_dropped += 1;
                }
                !dropped
            });
        }
        stats.simple_name_edges_kept += parsed.edges.len();
        let _ = original_edge_count;

        if let Some(scip_edges) = edges_by_caller_file.remove(&parsed.file) {
            stats.project_files_with_scip_edges += 1;
            for ed in scip_edges {
                parsed.edges.push(RawCallEdge {
                    caller_simple_name: ed.caller_simple,
                    caller_line: 0,
                    line: ed.line,
                    source: ed.source,
                    confidence: ed.confidence,
                    target: RawTarget::Simple {
                        callee_simple_name: ed.callee_simple,
                    },
                });
            }
        }
        project_files.push(parsed);
    }

    if !edges_by_caller_file.is_empty() {
        for (file, scip_edges) in edges_by_caller_file {
            let mut synthetic = ParsedFile {
                file,
                nodes: Vec::new(),
                edges: Vec::new(),
            };
            stats.project_files_with_scip_edges += 1;
            for ed in scip_edges {
                synthetic.edges.push(RawCallEdge {
                    caller_simple_name: ed.caller_simple,
                    caller_line: 0,
                    line: ed.line,
                    source: ed.source,
                    confidence: ed.confidence,
                    target: RawTarget::Simple {
                        callee_simple_name: ed.callee_simple,
                    },
                });
            }
            project_files.push(synthetic);
        }
    }

    let external_file = if scip.external_callees.is_empty() {
        None
    } else {
        let nodes: Vec<RawFnNode> = scip
            .external_callees
            .iter()
            .map(|c| RawFnNode {
                simple_name: c.symbol.clone(),
                line: 0,
                is_async: false,
                is_method: false,
                loc: 0,
                fingerprint: blake3_truncated(c.symbol.as_bytes()),
            })
            .collect();
        stats.external_node_count = nodes.len();
        Some(ParsedFile {
            file: PathBuf::from(EXTERN_FILE_LABEL),
            nodes,
            edges: Vec::new(),
        })
    };

    MergedProject {
        project_files,
        external_file,
        stats,
    }
}

fn promote_scip_fns_to_simple_files(
    project_root: &Path,
    simple: &mut ProjectParse,
    scip: &ScipResult,
) {
    let mut new_files: BTreeMap<PathBuf, ParsedFile> = BTreeMap::new();
    for fn_def in &scip.functions {
        let abs = absolute_for(project_root, &fn_def.file);
        let simple_name = simple_name_from_scip_symbol(&fn_def.symbol);
        if simple_name.is_empty() {
            continue;
        }
        let raw = codex_call_graph_store::RawFnNode {
            simple_name: simple_name.clone(),
            line: fn_def.line,
            is_async: false,
            is_method: false,
            loc: 0,
            fingerprint: blake3_truncated(fn_def.symbol.as_bytes()),
        };
        let mut placed = false;
        for parsed in simple.files.iter_mut() {
            if parsed.file == abs {
                if !parsed.nodes.iter().any(|n| n.simple_name == simple_name) {
                    parsed.nodes.push(raw.clone());
                }
                placed = true;
                break;
            }
        }
        if placed {
            continue;
        }
        let entry = new_files.entry(abs.clone()).or_insert_with(|| ParsedFile {
            file: abs.clone(),
            nodes: Vec::new(),
            edges: Vec::new(),
        });
        if !entry.nodes.iter().any(|n| n.simple_name == simple_name) {
            entry.nodes.push(raw);
        }
    }
    for (_path, parsed) in new_files {
        simple.files.push(parsed);
    }
}

fn absolute_for(project_root: &Path, relative: &Path) -> PathBuf {
    if relative.is_absolute() {
        relative.to_path_buf()
    } else {
        project_root.join(relative)
    }
}

fn simple_name_from_scip_symbol(symbol: &str) -> String {
    let descriptor = symbol.rsplit(' ').next().unwrap_or(symbol);
    let trimmed = descriptor.trim_end_matches('.');
    if let Some(open) = trimmed.rfind('(') {
        let before = &trimmed[..open];
        let last: String = before
            .chars()
            .rev()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect::<String>()
            .chars()
            .rev()
            .collect();
        if !last.is_empty() {
            return last;
        }
    }
    let cleaned = trimmed.trim_end_matches(['(', ')', '#', '/']);
    cleaned
        .chars()
        .rev()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect::<String>()
        .chars()
        .rev()
        .collect()
}

fn blake3_truncated(bytes: &[u8]) -> [u8; 16] {
    let hash = blake3::hash(bytes);
    let mut out = [0u8; 16];
    out.copy_from_slice(&hash.as_bytes()[..16]);
    out
}

struct PendingScipEdge {
    caller_simple: String,
    callee_simple: String,
    line: u32,
    source: EdgeSource,
    confidence: f32,
}
