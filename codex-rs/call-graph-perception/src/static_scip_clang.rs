use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::error::PerceptionError;
use crate::static_scip::{parse_scip_file, ScipResult};

const SCIP_CLANG_DEFAULT_TIMEOUT: Duration = Duration::from_secs(600);
const SCIP_CLANG_POLL_INTERVAL: Duration = Duration::from_millis(500);

#[derive(Debug, Clone)]
pub struct ScipClangOptions {
    pub timeout: Duration,
    pub binary: Option<PathBuf>,
    pub compdb_path: Option<PathBuf>,
    pub output_path: Option<PathBuf>,
    pub jobs: Option<u32>,
}

impl Default for ScipClangOptions {
    fn default() -> Self {
        Self {
            timeout: SCIP_CLANG_DEFAULT_TIMEOUT,
            binary: None,
            compdb_path: None,
            output_path: None,
            jobs: None,
        }
    }
}

pub fn locate_scip_clang(explicit: Option<&Path>) -> Result<PathBuf, PerceptionError> {
    if let Some(path) = explicit {
        if path.is_file() && probe(path) {
            return Ok(path.to_path_buf());
        }
        return Err(PerceptionError::RustAnalyzerMissing {
            hint: format!("explicit scip-clang path missing or non-functional: {}", path.display()),
        });
    }
    if let Ok(path) = std::env::var("CLAW_SCIP_CLANG") {
        let pb = PathBuf::from(&path);
        if pb.is_file() && probe(&pb) {
            return Ok(pb);
        }
    }
    if let Ok(path) = which("scip-clang") {
        if probe(&path) {
            return Ok(path);
        }
    }
    for candidate in candidate_install_paths() {
        if candidate.is_file() && probe(&candidate) {
            return Ok(candidate);
        }
    }
    Err(PerceptionError::RustAnalyzerMissing {
        hint: format!(
            "tried CLAW_SCIP_CLANG env, PATH, and known install locations ({}). \
             Install from https://github.com/sourcegraph/scip-clang/releases or set CLAW_SCIP_CLANG.",
            candidate_install_hint()
        ),
    })
}

fn candidate_install_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Some(home) = std::env::var_os("HOME") {
        let home = PathBuf::from(home);
        paths.push(home.join(".local").join("bin").join("scip-clang"));
        paths.push(home.join("bin").join("scip-clang"));
    }
    paths.push(PathBuf::from("/usr/local/bin/scip-clang"));
    paths.push(PathBuf::from("/opt/homebrew/bin/scip-clang"));
    paths.push(PathBuf::from("/opt/local/bin/scip-clang"));
    if cfg!(windows) {
        if let Ok(localappdata) = std::env::var("LOCALAPPDATA") {
            paths.push(PathBuf::from(localappdata).join("scip-clang").join("scip-clang.exe"));
        }
        if let Ok(programfiles) = std::env::var("ProgramFiles") {
            paths.push(PathBuf::from(programfiles).join("scip-clang").join("scip-clang.exe"));
        }
    }
    paths
}

fn candidate_install_hint() -> String {
    candidate_install_paths()
        .iter()
        .map(|p| p.display().to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

fn probe(path: &Path) -> bool {
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

pub fn run_scip_clang(
    project_root: &Path,
    options: &ScipClangOptions,
) -> Result<ScipResult, PerceptionError> {
    let started = Instant::now();
    let bin = locate_scip_clang(options.binary.as_deref())?;
    let compdb = options
        .compdb_path
        .clone()
        .or_else(|| {
            let candidate = project_root.join("compile_commands.json");
            candidate.is_file().then_some(candidate)
        })
        .or_else(|| {
            let candidate = project_root.join("build").join("compile_commands.json");
            candidate.is_file().then_some(candidate)
        })
        .ok_or_else(|| PerceptionError::RustAnalyzerMissing {
            hint: "C/C++ projects need a compile_commands.json (CMake: -DCMAKE_EXPORT_COMPILE_COMMANDS=ON, \
                   or use bear / intercept-build).  Tried project_root and project_root/build."
                .to_string(),
        })?;
    let scip_path = options
        .output_path
        .clone()
        .unwrap_or_else(|| project_root.join("index.scip"));

    let mut cmd = Command::new(&bin);
    cmd.arg("--compdb-path").arg(&compdb);
    cmd.arg("--index-output-path").arg(&scip_path);
    if let Some(j) = options.jobs {
        cmd.arg("-j").arg(j.to_string());
    }
    cmd.current_dir(project_root);
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());

    let mut child = cmd.spawn().map_err(|err| PerceptionError::Io {
        path: bin.clone(),
        source: err,
    })?;

    let deadline = started + options.timeout;
    loop {
        match child.try_wait() {
            Ok(Some(_status)) => break,
            Ok(None) => {
                if Instant::now() > deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(PerceptionError::ScipTimeout {
                        timeout: options.timeout,
                    });
                }
                std::thread::sleep(SCIP_CLANG_POLL_INTERVAL);
            }
            Err(err) => {
                return Err(PerceptionError::Io {
                    path: bin.clone(),
                    source: err,
                });
            }
        }
    }
    let output = child.wait_with_output().map_err(|err| PerceptionError::Io {
        path: bin.clone(),
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn locate_scip_clang_explicit_path_must_exist() {
        let dir = tempdir().expect("tempdir");
        let bogus = dir.path().join("nope");
        let result = locate_scip_clang(Some(&bogus));
        assert!(result.is_err());
    }

    #[test]
    fn locate_scip_clang_rejects_non_functional_binary() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("scip-clang");
        fs::write(&path, b"#!/bin/sh\nexit 1\n").expect("write stub");
        let mut perms = fs::metadata(&path).expect("meta").permissions();
        use std::os::unix::fs::PermissionsExt;
        perms.set_mode(0o755);
        fs::set_permissions(&path, perms).expect("chmod");
        let result = locate_scip_clang(Some(&path));
        assert!(result.is_err(), "non-zero exit must be rejected");
    }
}
