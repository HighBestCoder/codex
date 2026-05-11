use std::collections::HashSet;
use std::path::PathBuf;

use codex_call_graph_store::{EdgeSource, GraphStore, ResolvedTarget};
use codex_call_graph_tools::pgs_path_for;
use codex_protocol::models::{ContentItem, ResponseItem};

use crate::client_common::Prompt;

const MAX_IDENTIFIER_LOOKUPS: usize = 12;
const MAX_NODES_RENDERED: usize = 6;
const MAX_NEIGHBORS_PER_DIRECTION: usize = 3;

pub fn decorate_instructions(base: &str, prompt: &Prompt) -> String {
    let cwd = match std::env::current_dir() {
        Ok(p) => p,
        Err(_) => return base.to_string(),
    };
    let pgs_path = pgs_path_for(&cwd);
    if !pgs_path.exists() {
        return append_no_graph_hint(base);
    }
    let store = match GraphStore::open_with_retry(&pgs_path, 5, 100) {
        Ok(s) => s,
        Err(_) => return base.to_string(),
    };
    let user_text = latest_user_text(&prompt.input);
    if user_text.is_empty() {
        return base.to_string();
    }
    let candidates = candidate_identifiers(&user_text);
    let rendered = render_graph_context(&store, &candidates);
    if rendered.is_empty() {
        return base.to_string();
    }
    format!(
        "{base}\n\n## Project Call Graph (auto-injected)\n\
         The following symbols mentioned in the user's most recent message were found in the project's call graph (built by `/graph-map`). \
         Use this context to ground your answer; call `graph_why` / `graph_plan` for deeper queries.\n\n{rendered}"
    )
}

fn append_no_graph_hint(base: &str) -> String {
    format!(
        "{base}\n\n## Project Call Graph\nNo call graph has been built for this project yet. Run `/graph-map` (or invoke the `graph_map` tool) to index it before asking code-structure questions."
    )
}

fn latest_user_text(input: &[ResponseItem]) -> String {
    for item in input.iter().rev() {
        if let ResponseItem::Message { role, content, .. } = item {
            if role != "user" {
                continue;
            }
            let mut buf = String::new();
            for piece in content {
                if let ContentItem::InputText { text } = piece {
                    if !buf.is_empty() {
                        buf.push('\n');
                    }
                    buf.push_str(text);
                }
            }
            if !buf.is_empty() {
                return buf;
            }
        }
    }
    String::new()
}

fn candidate_identifiers(text: &str) -> Vec<String> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut out: Vec<String> = Vec::new();
    let mut iter = text.chars().peekable();
    let mut buf = String::new();
    while let Some(ch) = iter.next() {
        if ch.is_ascii_alphabetic() || ch == '_' {
            buf.push(ch);
            while let Some(&next) = iter.peek() {
                if next.is_ascii_alphanumeric() || next == '_' {
                    buf.push(next);
                    iter.next();
                } else {
                    break;
                }
            }
            let candidate = std::mem::take(&mut buf);
            if !is_likely_identifier(&candidate) {
                continue;
            }
            if seen.insert(candidate.clone()) {
                out.push(candidate);
                if out.len() >= MAX_IDENTIFIER_LOOKUPS {
                    return out;
                }
            }
        }
    }
    out
}

fn is_likely_identifier(token: &str) -> bool {
    if token.len() < 3 || token.len() > 64 {
        return false;
    }
    if !token.chars().any(|c| c.is_ascii_alphabetic()) {
        return false;
    }
    !STOPWORDS.contains(&token.to_ascii_lowercase().as_str())
}

const STOPWORDS: &[&str] = &[
    "the", "and", "for", "with", "from", "this", "that", "into", "your", "you",
    "what", "where", "when", "which", "while", "function", "method", "class",
    "struct", "module", "file", "code", "call", "calls", "called", "caller",
    "callers", "callee", "callees", "tell", "show", "find", "list", "use",
    "using", "report", "summary", "summarize", "should", "would", "could",
    "please", "thanks", "test", "tests",
];

fn render_graph_context(store: &GraphStore, idents: &[String]) -> String {
    let mut blocks: Vec<String> = Vec::new();
    for ident in idents {
        let nodes = match store.nodes_by_name(ident) {
            Ok(n) => n,
            Err(_) => continue,
        };
        if nodes.is_empty() {
            continue;
        }
        for node in nodes.into_iter().take(2) {
            let callers = collect_neighbors(store, node.id, NeighborDirection::Callers);
            let callees = collect_neighbors(store, node.id, NeighborDirection::Callees);
            let mut block = format!(
                "- `{}` @ {}:{}",
                node.simple_name,
                node.file.display(),
                node.line,
            );
            if !callers.is_empty() {
                block.push_str("\n    callers: ");
                block.push_str(&callers.join(", "));
            }
            if !callees.is_empty() {
                block.push_str("\n    callees: ");
                block.push_str(&callees.join(", "));
            }
            blocks.push(block);
            if blocks.len() >= MAX_NODES_RENDERED {
                return blocks.join("\n");
            }
        }
    }
    blocks.join("\n")
}

enum NeighborDirection {
    Callers,
    Callees,
}

fn collect_neighbors(
    store: &GraphStore,
    node_id: codex_call_graph_store::NodeId,
    direction: NeighborDirection,
) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let edges = match direction {
        NeighborDirection::Callers => store.in_edges(node_id),
        NeighborDirection::Callees => store.out_edges(node_id),
    };
    let Ok(edges) = edges else { return out };
    for edge in edges.into_iter().take(MAX_NEIGHBORS_PER_DIRECTION) {
        let label = match direction {
            NeighborDirection::Callers => match store.node(edge.caller_id) {
                Ok(Some(node)) => format_neighbor_label(&node.simple_name, edge.source),
                _ => continue,
            },
            NeighborDirection::Callees => match &edge.target {
                ResolvedTarget::Simple { callee_name } => format_neighbor_label(callee_name, edge.source),
                ResolvedTarget::Resolved { callee_id } => match store.node(*callee_id) {
                    Ok(Some(node)) => format_neighbor_label(&node.simple_name, edge.source),
                    _ => continue,
                },
            },
        };
        out.push(label);
    }
    out
}

fn format_neighbor_label(name: &str, source: EdgeSource) -> String {
    let tag = match source {
        EdgeSource::SimpleName => "",
        EdgeSource::ScipInternal => " [scip]",
        EdgeSource::ScipExternal => " [extern]",
        EdgeSource::Dynamic => " [dyn]",
    };
    format!("`{name}`{tag}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stopwords_filter_drops_english_chatter() {
        assert!(!is_likely_identifier("the"));
        assert!(!is_likely_identifier("function"));
        assert!(!is_likely_identifier("test"));
        assert!(is_likely_identifier("ValidateCollectionName"));
        assert!(is_likely_identifier("foo_bar"));
    }

    #[test]
    fn candidate_identifiers_extracts_camel_and_snake_case_only() {
        let text = "Tell me what calls ValidateCollectionName and foo_bar in src/lib.rs";
        let got = candidate_identifiers(text);
        assert!(got.contains(&"ValidateCollectionName".to_string()));
        assert!(got.contains(&"foo_bar".to_string()));
        assert!(!got.contains(&"and".to_string()));
        assert!(!got.contains(&"tell".to_string()));
    }

    #[test]
    fn append_no_graph_hint_keeps_base_and_appends_actionable_note() {
        let result = append_no_graph_hint("you are codex");
        assert!(result.starts_with("you are codex"));
        assert!(result.contains("/graph-map"));
    }

    #[test]
    fn latest_user_text_skips_assistant_messages_and_returns_most_recent_user() {
        let input = vec![
            ResponseItem::Message {
                id: None,
                role: "user".into(),
                content: vec![ContentItem::InputText { text: "old user".into() }],
                phase: None,
            },
            ResponseItem::Message {
                id: None,
                role: "assistant".into(),
                content: vec![ContentItem::OutputText { text: "old assistant".into() }],
                phase: None,
            },
            ResponseItem::Message {
                id: None,
                role: "user".into(),
                content: vec![ContentItem::InputText { text: "newest".into() }],
                phase: None,
            },
        ];
        assert_eq!(latest_user_text(&input), "newest");
    }

    #[allow(dead_code)]
    fn _ensure_pathbuf_unused_warning_suppressed() -> Option<PathBuf> {
        None
    }
}
