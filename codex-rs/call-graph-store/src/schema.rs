use std::path::PathBuf;

use serde::{Deserialize, Serialize};

pub type StringId = u32;
pub type NodeId = u32;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EdgeSource {
    SimpleName,
    ScipInternal,
    ScipExternal,
    Dynamic,
}

impl EdgeSource {
    #[must_use]
    pub fn default_confidence(self) -> f32 {
        match self {
            Self::SimpleName => 0.85,
            Self::ScipInternal => 0.99,
            Self::ScipExternal => 1.0,
            Self::Dynamic => 1.0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct NodeMetrics {
    pub cyclomatic: u32,
    pub in_deg: u32,
    pub out_deg: u32,
    pub pagerank: f32,
    pub hot_score: f32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StoredFnNode {
    pub id: NodeId,
    pub simple_name: StringId,
    pub file: StringId,
    pub line: u32,
    pub is_async: bool,
    pub is_method: bool,
    pub loc: u32,
    pub fingerprint: [u8; 16],
    pub semantic_brief: Option<StringId>,
    pub metrics: NodeMetrics,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum EdgeTarget {
    Simple { callee_name: StringId },
    Resolved { callee_id: NodeId },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StoredEdge {
    pub caller_id: NodeId,
    pub line: u32,
    pub source: EdgeSource,
    pub confidence: f32,
    pub target: EdgeTarget,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FnNode {
    pub id: NodeId,
    pub simple_name: String,
    pub file: PathBuf,
    pub line: u32,
    pub is_async: bool,
    pub is_method: bool,
    pub loc: u32,
    pub fingerprint: [u8; 16],
    pub semantic_brief: Option<String>,
    pub metrics: NodeMetrics,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CallEdge {
    pub caller_id: NodeId,
    pub line: u32,
    pub source: EdgeSource,
    pub confidence: f32,
    pub target: ResolvedTarget,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ResolvedTarget {
    Simple { callee_name: String },
    Resolved { callee_id: NodeId },
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct RawFnNode {
    pub simple_name: String,
    pub line: u32,
    pub is_async: bool,
    pub is_method: bool,
    pub loc: u32,
    pub fingerprint: [u8; 16],
}

#[derive(Debug, Clone, PartialEq)]
pub struct RawCallEdge {
    pub caller_simple_name: String,
    pub caller_line: u32,
    pub line: u32,
    pub source: EdgeSource,
    pub confidence: f32,
    pub target: RawTarget,
}

#[derive(Debug, Clone, PartialEq)]
pub enum RawTarget {
    Simple { callee_simple_name: String },
    Resolved { callee_id: NodeId },
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct ParsedFile {
    pub file: PathBuf,
    pub nodes: Vec<RawFnNode>,
    pub edges: Vec<RawCallEdge>,
}
