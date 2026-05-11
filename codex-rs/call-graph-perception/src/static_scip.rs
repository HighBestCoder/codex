use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use scip::types::{Index, SymbolRole};

use crate::error::PerceptionError;

const SCIP_DEFAULT_TIMEOUT: Duration = Duration::from_secs(120);
const SCIP_POLL_INTERVAL: Duration = Duration::from_millis(200);

#[derive(Debug, Clone, Default)]
pub struct ScipResult {
    pub functions: Vec<ScipFn>,
    pub external_callees: Vec<ExternalCallee>,
    pub edges: Vec<ScipEdge>,
    pub stats: ScipStats,
}

#[derive(Debug, Clone)]
pub struct ScipFn {
    pub symbol: String,
    pub display: String,
    pub file: PathBuf,
    pub line: u32,
}

#[derive(Debug, Clone)]
pub struct ExternalCallee {
    pub symbol: String,
    pub display: String,
    pub crate_hint: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ScipEdge {
    pub caller_symbol: String,
    pub callee_symbol: String,
    pub line: u32,
    pub is_external: bool,
}

#[derive(Debug, Clone, Default)]
pub struct ScipStats {
    pub document_count: usize,
    pub raw_occurrence_count: usize,
    pub elapsed: Duration,
    pub scip_file_size_bytes: u64,
}

#[derive(Debug, Clone)]
pub struct ScipOptions {
    pub timeout: Duration,
    pub rust_analyzer: Option<PathBuf>,
    pub output_path: Option<PathBuf>,
    pub exclude_vendored_libraries: bool,
}

impl Default for ScipOptions {
    fn default() -> Self {
        Self {
            timeout: SCIP_DEFAULT_TIMEOUT,
            rust_analyzer: None,
            output_path: None,
            exclude_vendored_libraries: true,
        }
    }
}

pub fn locate_rust_analyzer(explicit: Option<&Path>) -> Result<PathBuf, PerceptionError> {
    if let Some(path) = explicit {
        if path.is_file() {
            return Ok(path.to_path_buf());
        }
        return Err(PerceptionError::RustAnalyzerMissing {
            hint: format!("explicit path does not exist: {}", path.display()),
        });
    }
    if let Ok(path) = std::env::var("RUST_ANALYZER") {
        let pb = PathBuf::from(&path);
        if pb.is_file() && probe_rust_analyzer(&pb) {
            return Ok(pb);
        }
    }
    if let Ok(path) = which("rust-analyzer") {
        if probe_rust_analyzer(&path) {
            return Ok(path);
        }
    }
    if let Some(home) = std::env::var_os("HOME") {
        let extensions = PathBuf::from(home)
            .join(".vscode")
            .join("extensions");
        if let Ok(entries) = std::fs::read_dir(&extensions) {
            let mut candidates: Vec<PathBuf> = entries
                .filter_map(Result::ok)
                .filter_map(|entry| {
                    let name = entry.file_name();
                    let name_str = name.to_string_lossy();
                    if name_str.starts_with("rust-lang.rust-analyzer-") {
                        let candidate = entry.path().join("server").join("rust-analyzer");
                        if candidate.is_file() {
                            return Some(candidate);
                        }
                    }
                    None
                })
                .collect();
            candidates.sort();
            if let Some(latest) = candidates.into_iter().next_back() {
                if probe_rust_analyzer(&latest) {
                    return Ok(latest);
                }
            }
        }
    }
    Err(PerceptionError::RustAnalyzerMissing {
        hint: "tried RUST_ANALYZER env, PATH, and ~/.vscode/extensions/rust-lang.rust-analyzer-*. \
               Install via `rustup component add rust-analyzer` or set the RUST_ANALYZER env var."
            .to_string(),
    })
}

fn probe_rust_analyzer(path: &Path) -> bool {
    Command::new(path)
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn which(bin: &str) -> Result<PathBuf, std::io::Error> {
    let output = Command::new("which").arg(bin).output()?;
    if !output.status.success() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("{bin} not on PATH"),
        ));
    }
    let path = String::from_utf8_lossy(&output.stdout);
    Ok(PathBuf::from(path.trim()))
}

pub fn run_scip(project_root: &Path, options: &ScipOptions) -> Result<ScipResult, PerceptionError> {
    let started = Instant::now();
    let ra = locate_rust_analyzer(options.rust_analyzer.as_deref())?;

    let scip_path = options
        .output_path
        .clone()
        .unwrap_or_else(|| std::env::temp_dir().join("claw-scip.scip"));

    let mut cmd = Command::new(&ra);
    cmd.arg("scip").arg(".");
    cmd.arg("--output").arg(&scip_path);
    if options.exclude_vendored_libraries {
        cmd.arg("--exclude-vendored-libraries");
    }
    cmd.current_dir(project_root);
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());

    let mut child = cmd.spawn().map_err(|err| PerceptionError::Io {
        path: ra.clone(),
        source: err,
    })?;

    let timeout = options.timeout;
    let deadline = started + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(_status)) => break,
            Ok(None) => {
                if Instant::now() > deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(PerceptionError::ScipTimeout { timeout });
                }
                std::thread::sleep(SCIP_POLL_INTERVAL);
            }
            Err(err) => {
                return Err(PerceptionError::Io {
                    path: ra.clone(),
                    source: err,
                });
            }
        }
    }
    let output = child.wait_with_output().map_err(|err| PerceptionError::Io {
        path: ra.clone(),
        source: err,
    })?;

    if !output.status.success() {
        return Err(PerceptionError::ScipFailed {
            status: output.status.to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        });
    }

    parse_scip_file(&scip_path, started)
}

pub fn parse_scip_file(scip_path: &Path, started: Instant) -> Result<ScipResult, PerceptionError> {
    let bytes = std::fs::read(scip_path).map_err(|err| PerceptionError::Io {
        path: scip_path.to_path_buf(),
        source: err,
    })?;
    let scip_file_size_bytes = bytes.len() as u64;
    let idx: Index = protobuf::Message::parse_from_bytes(&bytes)
        .map_err(|err| PerceptionError::ScipParse(err.to_string()))?;

    let mut stats = ScipStats {
        document_count: idx.documents.len(),
        scip_file_size_bytes,
        ..ScipStats::default()
    };
    let mut functions: Vec<ScipFn> = Vec::new();
    let mut function_keys: HashSet<String> = HashSet::new();
    let mut edges: Vec<ScipEdge> = Vec::new();
    // Dedup edges across translation units: scip-clang emits the same header
    // (and its calls) once per TU that includes it. Without dedup, fmt-style
    // projects produce ~Nx duplicate edges where N = number of TUs including
    // a hot header. Key by (caller_symbol, callee_symbol, line).
    let mut edge_keys: HashSet<(String, String, u32)> = HashSet::new();
    let mut external_seen: HashSet<String> = HashSet::new();
    let mut external_callees: Vec<ExternalCallee> = Vec::new();

    for doc in &idx.documents {
        let file = PathBuf::from(&doc.relative_path);
        let mut local_defs: Vec<(String, u32)> = Vec::new();
        for occ in &doc.occurrences {
            stats.raw_occurrence_count += 1;
            if !is_definition(occ.symbol_roles) {
                continue;
            }
            if !looks_like_function(&occ.symbol) {
                continue;
            }
            let Some(start_line) = first_line_of_range(&occ.range) else {
                continue;
            };
            local_defs.push((occ.symbol.clone(), start_line));
            if function_keys.insert(occ.symbol.clone()) {
                functions.push(ScipFn {
                    symbol: occ.symbol.clone(),
                    display: pretty_symbol(&occ.symbol),
                    file: file.clone(),
                    line: start_line,
                });
            }
        }
        local_defs.sort_by_key(|(_, line)| *line);

        for occ in &doc.occurrences {
            if is_definition(occ.symbol_roles) {
                continue;
            }
            if !looks_like_function(&occ.symbol) {
                continue;
            }
            let Some(line) = first_line_of_range(&occ.range) else {
                continue;
            };

            let caller = local_defs
                .iter()
                .filter(|(_, def_line)| *def_line <= line)
                .next_back()
                .map(|(sym, _)| sym.clone());

            let Some(caller) = caller else { continue };
            if caller == occ.symbol {
                continue;
            }
            let is_external = !function_keys.contains(&occ.symbol)
                && !local_defs.iter().any(|(s, _)| s == &occ.symbol);

            if is_external && external_seen.insert(occ.symbol.clone()) {
                external_callees.push(ExternalCallee {
                    symbol: occ.symbol.clone(),
                    display: pretty_symbol(&occ.symbol),
                    crate_hint: extract_crate_hint(&occ.symbol),
                });
            }

            edges.push(ScipEdge {
                caller_symbol: caller,
                callee_symbol: occ.symbol.clone(),
                line,
                is_external,
            });
        }
    }

    edges.retain(|e| edge_keys.insert((e.caller_symbol.clone(), e.callee_symbol.clone(), e.line)));
    deduplicate_functions(&mut functions);
    stats.elapsed = started.elapsed();
    Ok(ScipResult {
        functions,
        external_callees,
        edges,
        stats,
    })
}

fn deduplicate_functions(functions: &mut Vec<ScipFn>) {
    let mut seen: BTreeMap<String, ScipFn> = BTreeMap::new();
    for f in std::mem::take(functions) {
        seen.entry(f.symbol.clone()).or_insert(f);
    }
    *functions = seen.into_values().collect();
}

fn is_definition(roles: i32) -> bool {
    roles & SymbolRole::Definition as i32 != 0
}

fn looks_like_function(symbol: &str) -> bool {
    if symbol.contains("().") {
        return true;
    }
    let trimmed = symbol.trim_end_matches('.');
    if !trimmed.ends_with(')') {
        return false;
    }
    let body = &trimmed[..trimmed.len() - 1];
    if let Some(open) = body.rfind('(') {
        let before = &body[..open];
        let last = before
            .chars()
            .rev()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect::<String>();
        return !last.is_empty();
    }
    false
}

fn first_line_of_range(range: &[i32]) -> Option<u32> {
    match range {
        [sl, _, _, _] | [sl, _, _] => Some(*sl as u32),
        _ => None,
    }
}

fn pretty_symbol(symbol: &str) -> String {
    let descriptor = symbol.rsplit(' ').next().unwrap_or(symbol);
    let trimmed = descriptor.trim_end_matches('.');
    if let Some(open) = trimmed.rfind('(') {
        let before = &trimmed[..open];
        let last: String = before
            .chars()
            .rev()
            .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == ':' || *c == '#')
            .collect::<String>()
            .chars()
            .rev()
            .collect();
        if !last.is_empty() {
            return last;
        }
    }
    descriptor.trim_end_matches(['.', '(', ')']).to_string()
}

fn extract_crate_hint(symbol: &str) -> Option<String> {
    let parts: Vec<&str> = symbol.split(' ').collect();
    if parts.len() >= 3 && parts[0] == "rust-analyzer" && parts[1] == "cargo" {
        return Some(parts[2].to_string());
    }
    if parts.first().is_some_and(|p| *p == "cxx") {
        return Some("cxx".into());
    }
    None
}
