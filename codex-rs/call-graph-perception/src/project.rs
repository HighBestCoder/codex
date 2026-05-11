use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectKind {
    Rust {
        manifest: PathBuf,
    },
    CppCmake {
        compile_db: PathBuf,
    },
    CppNoCompdb {
        reason: String,
    },
    Mixed(Vec<ProjectKind>),
    Unknown {
        reason: String,
    },
}

impl ProjectKind {
    pub fn is_rust(&self) -> bool {
        matches!(self, Self::Rust { .. })
    }
    pub fn is_cpp(&self) -> bool {
        matches!(self, Self::CppCmake { .. } | Self::CppNoCompdb { .. })
    }
    pub fn label(&self) -> &'static str {
        match self {
            Self::Rust { .. } => "rust",
            Self::CppCmake { .. } => "cpp-cmake",
            Self::CppNoCompdb { .. } => "cpp-no-compdb",
            Self::Mixed(_) => "mixed",
            Self::Unknown { .. } => "unknown",
        }
    }
}

pub fn detect(root: &Path) -> ProjectKind {
    let mut kinds: Vec<ProjectKind> = Vec::new();
    if let Some(rust) = detect_rust(root) {
        kinds.push(rust);
    }
    if let Some(cpp) = detect_cpp(root) {
        kinds.push(cpp);
    }
    match kinds.len() {
        0 => ProjectKind::Unknown {
            reason: "no Cargo.toml, compile_commands.json, CMakeLists.txt, or Makefile found".into(),
        },
        1 => kinds.into_iter().next().unwrap(),
        _ => ProjectKind::Mixed(kinds),
    }
}

fn detect_rust(root: &Path) -> Option<ProjectKind> {
    let manifest = root.join("Cargo.toml");
    if manifest.is_file() {
        return Some(ProjectKind::Rust { manifest });
    }
    None
}

fn detect_cpp(root: &Path) -> Option<ProjectKind> {
    for candidate in [
        root.join("compile_commands.json"),
        root.join("build").join("compile_commands.json"),
    ] {
        if candidate.is_file() {
            return Some(ProjectKind::CppCmake { compile_db: candidate });
        }
    }
    if root.join("CMakeLists.txt").is_file() {
        return Some(ProjectKind::CppNoCompdb {
            reason: "CMakeLists.txt found but no compile_commands.json. \
                     Run `cmake -DCMAKE_EXPORT_COMPILE_COMMANDS=ON` in a build dir."
                .into(),
        });
    }
    if root.join("Makefile").is_file() || root.join("configure.ac").is_file() {
        return Some(ProjectKind::CppNoCompdb {
            reason: "Makefile/autotools found but no compile_commands.json. \
                     Use bear / intercept-build to generate one."
                .into(),
        });
    }
    let has_source = walk_for_extensions(root, &["cpp", "cc", "cxx", "c"], 3);
    if has_source {
        return Some(ProjectKind::CppNoCompdb {
            reason: "C/C++ source files found but no build system metadata."
                .into(),
        });
    }
    None
}

fn walk_for_extensions(root: &Path, extensions: &[&str], max_depth: usize) -> bool {
    fn inner(path: &Path, extensions: &[&str], depth: usize, max: usize) -> bool {
        let Ok(entries) = std::fs::read_dir(path) else { return false };
        for entry in entries.filter_map(Result::ok) {
            let p = entry.path();
            if p.is_dir() {
                let name = p
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                if name == "target" || name == ".git" || name == "node_modules" {
                    continue;
                }
                if depth + 1 < max && inner(&p, extensions, depth + 1, max) {
                    return true;
                }
            } else if let Some(ext) = p.extension().and_then(|e| e.to_str()) {
                if extensions.contains(&ext) {
                    return true;
                }
            }
        }
        false
    }
    inner(root, extensions, 0, max_depth)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn detects_rust_project_via_cargo_toml() {
        let dir = tempdir().expect("tempdir");
        fs::write(dir.path().join("Cargo.toml"), "[package]\nname=\"x\"\n").expect("write");
        let kind = detect(dir.path());
        assert!(kind.is_rust());
    }

    #[test]
    fn detects_cpp_with_compile_commands_at_root() {
        let dir = tempdir().expect("tempdir");
        fs::write(dir.path().join("compile_commands.json"), "[]").expect("write");
        let kind = detect(dir.path());
        assert!(kind.is_cpp());
        assert_eq!(kind.label(), "cpp-cmake");
    }

    #[test]
    fn detects_cpp_with_compile_commands_in_build_dir() {
        let dir = tempdir().expect("tempdir");
        fs::create_dir_all(dir.path().join("build")).expect("dir");
        fs::write(
            dir.path().join("build").join("compile_commands.json"),
            "[]",
        )
        .expect("write");
        let kind = detect(dir.path());
        assert!(kind.is_cpp());
    }

    #[test]
    fn detects_cmake_without_compdb_with_hint() {
        let dir = tempdir().expect("tempdir");
        fs::write(
            dir.path().join("CMakeLists.txt"),
            "cmake_minimum_required(VERSION 3.10)\n",
        )
        .expect("write");
        let kind = detect(dir.path());
        match kind {
            ProjectKind::CppNoCompdb { reason } => assert!(reason.contains("compile_commands")),
            other => panic!("expected CppNoCompdb, got {other:?}"),
        }
    }

    #[test]
    fn detects_mixed_when_both_cargo_toml_and_compile_commands_present() {
        let dir = tempdir().expect("tempdir");
        fs::write(dir.path().join("Cargo.toml"), "[package]\n").expect("write");
        fs::write(dir.path().join("compile_commands.json"), "[]").expect("write");
        let kind = detect(dir.path());
        assert!(matches!(kind, ProjectKind::Mixed(_)));
        assert_eq!(kind.label(), "mixed");
    }

    #[test]
    fn detects_unknown_in_empty_dir() {
        let dir = tempdir().expect("tempdir");
        let kind = detect(dir.path());
        assert!(matches!(kind, ProjectKind::Unknown { .. }));
    }
}
