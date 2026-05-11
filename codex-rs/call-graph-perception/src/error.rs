use std::io;
use std::path::PathBuf;
use std::time::Duration;

#[derive(Debug, thiserror::Error)]
pub enum PerceptionError {
    #[error("io error reading {path}: {source}")]
    Io { path: PathBuf, source: io::Error },
    #[error("syn parse error in {path}: {source}")]
    Syn { path: PathBuf, source: syn::Error },
    #[error("rust-analyzer binary not found: {hint}")]
    RustAnalyzerMissing { hint: String },
    #[error("rust-analyzer scip exited with status {status}: {stderr}")]
    ScipFailed { status: String, stderr: String },
    #[error("rust-analyzer scip timed out after {timeout:?}")]
    ScipTimeout { timeout: Duration },
    #[error("scip protobuf parse error: {0}")]
    ScipParse(String),
}
