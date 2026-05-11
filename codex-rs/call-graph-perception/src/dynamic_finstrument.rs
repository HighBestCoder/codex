use std::path::{Path, PathBuf};
use std::process::Command;

const FINSTRUMENT_C_SOURCE: &str =
    include_str!("../resources/finstrument/claw_finstrument.c");

#[derive(Debug, thiserror::Error)]
pub enum FinstrumentError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("compiler {compiler:?} exited with status {status}: {stderr}")]
    CompilerFailed {
        compiler: String,
        status: String,
        stderr: String,
    },
    #[error("could not locate a C compiler ({tried:?})")]
    NoCompiler { tried: Vec<String> },
}

#[derive(Debug, Clone)]
pub struct BuildOptions {
    pub compiler: Option<PathBuf>,
    pub output_path: Option<PathBuf>,
    pub extra_cflags: Vec<String>,
}

impl Default for BuildOptions {
    fn default() -> Self {
        Self {
            compiler: None,
            output_path: None,
            extra_cflags: Vec::new(),
        }
    }
}

pub fn finstrument_source() -> &'static str {
    FINSTRUMENT_C_SOURCE
}

pub fn write_runtime_source(target_dir: &Path) -> Result<PathBuf, FinstrumentError> {
    std::fs::create_dir_all(target_dir)?;
    let path = target_dir.join("claw_finstrument.c");
    std::fs::write(&path, FINSTRUMENT_C_SOURCE)?;
    Ok(path)
}

pub fn build_runtime_so(
    target_dir: &Path,
    options: &BuildOptions,
) -> Result<PathBuf, FinstrumentError> {
    let source = write_runtime_source(target_dir)?;
    let compiler = pick_compiler(options.compiler.as_deref())?;
    let output_path = options
        .output_path
        .clone()
        .unwrap_or_else(|| target_dir.join("libclaw_finstrument.so"));

    let mut cmd = Command::new(&compiler);
    cmd.args([
        "-shared",
        "-fPIC",
        "-O2",
        "-fno-instrument-functions",
    ]);
    for flag in &options.extra_cflags {
        cmd.arg(flag);
    }
    cmd.arg(&source);
    cmd.arg("-ldl");
    cmd.arg("-pthread");
    cmd.arg("-o");
    cmd.arg(&output_path);

    let result = cmd.output()?;
    if !result.status.success() {
        return Err(FinstrumentError::CompilerFailed {
            compiler: compiler.display().to_string(),
            status: result.status.to_string(),
            stderr: String::from_utf8_lossy(&result.stderr).to_string(),
        });
    }
    Ok(output_path)
}

fn pick_compiler(explicit: Option<&Path>) -> Result<PathBuf, FinstrumentError> {
    if let Some(path) = explicit {
        if path.is_file() {
            return Ok(path.to_path_buf());
        }
    }
    let candidates = ["cc", "clang", "gcc"];
    for cand in &candidates {
        if let Some(path) = which(cand) {
            return Ok(path);
        }
    }
    Err(FinstrumentError::NoCompiler {
        tried: candidates.iter().map(|s| s.to_string()).collect(),
    })
}

fn which(bin: &str) -> Option<PathBuf> {
    Command::new("which")
        .arg(bin)
        .output()
        .ok()
        .and_then(|o| {
            if o.status.success() {
                Some(PathBuf::from(String::from_utf8_lossy(&o.stdout).trim()))
            } else {
                None
            }
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn finstrument_source_is_non_empty_and_includes_cyg_profile() {
        let src = finstrument_source();
        assert!(src.contains("__cyg_profile_func_enter"));
        assert!(src.contains("__cyg_profile_func_exit"));
        assert!(src.contains("CLAW_TRACE_OUTPUT"));
    }

    #[test]
    fn write_runtime_source_creates_c_file() {
        let dir = tempdir().expect("tempdir");
        let path = write_runtime_source(dir.path()).expect("write");
        assert!(path.is_file());
        let content = std::fs::read_to_string(&path).expect("read");
        assert!(content.contains("__cyg_profile_func_enter"));
    }

    #[test]
    fn build_runtime_so_produces_shared_library() {
        let dir = tempdir().expect("tempdir");
        let opts = BuildOptions::default();
        let so = match build_runtime_so(dir.path(), &opts) {
            Ok(p) => p,
            Err(FinstrumentError::NoCompiler { .. }) => {
                eprintln!("skip: no C compiler available in this environment");
                return;
            }
            Err(other) => panic!("build failed: {other:?}"),
        };
        assert!(so.is_file());
        let metadata = std::fs::metadata(&so).expect("metadata");
        assert!(metadata.len() > 1024, "shared library suspiciously small");
    }
}
