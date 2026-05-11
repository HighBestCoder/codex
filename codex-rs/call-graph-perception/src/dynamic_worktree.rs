use std::path::{Path, PathBuf};

use git2::{Repository, WorktreeAddOptions};

#[derive(Debug, thiserror::Error)]
pub enum WorktreeError {
    #[error("git error: {0}")]
    Git(#[from] git2::Error),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("not a git repository at {0}")]
    NotARepo(PathBuf),
}

pub struct TraceWorktree {
    pub path: PathBuf,
    pub name: String,
    repo_path: PathBuf,
    keep: bool,
}

impl TraceWorktree {
    pub fn create(repo_path: &Path) -> Result<Self, WorktreeError> {
        Self::create_under(repo_path, &default_worktree_root())
    }

    pub fn create_under(repo_path: &Path, parent: &Path) -> Result<Self, WorktreeError> {
        let repo = Repository::discover(repo_path)?;
        let workdir = repo
            .workdir()
            .ok_or_else(|| WorktreeError::NotARepo(repo_path.to_path_buf()))?
            .to_path_buf();
        std::fs::create_dir_all(parent)?;
        let suffix = unique_suffix();
        let name = format!("claw-trace-{suffix}");
        let target_path = parent.join(&name);
        let mut opts = WorktreeAddOptions::new();
        opts.lock(true);
        repo.worktree(&name, &target_path, Some(&opts))?;
        Ok(Self {
            path: target_path,
            name,
            repo_path: workdir,
            keep: false,
        })
    }

    pub fn keep(&mut self) {
        self.keep = true;
    }

    pub fn cleanup(&self) -> Result<(), WorktreeError> {
        let repo = Repository::discover(&self.repo_path)?;
        if let Ok(wt) = repo.find_worktree(&self.name) {
            let _ = wt.prune(None);
        }
        if self.path.exists() {
            std::fs::remove_dir_all(&self.path).ok();
        }
        Ok(())
    }
}

impl Drop for TraceWorktree {
    fn drop(&mut self) {
        if self.keep {
            return;
        }
        let _ = self.cleanup();
    }
}

fn default_worktree_root() -> PathBuf {
    if let Some(home) = std::env::var_os("HOME") {
        PathBuf::from(home).join(".claw").join("worktrees")
    } else {
        std::env::temp_dir().join("claw-worktrees")
    }
}

fn unique_suffix() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let pid = std::process::id();
    format!("{pid}-{now:x}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    use tempfile::tempdir;

    fn init_repo() -> tempfile::TempDir {
        let dir = tempdir().expect("tempdir");
        let root = dir.path();
        Command::new("git")
            .arg("init")
            .arg("-q")
            .arg("--initial-branch=main")
            .current_dir(root)
            .status()
            .expect("git init");
        Command::new("git")
            .args(["config", "user.email", "test@example.invalid"])
            .current_dir(root)
            .status()
            .expect("config");
        Command::new("git")
            .args(["config", "user.name", "Tester"])
            .current_dir(root)
            .status()
            .expect("config");
        std::fs::write(root.join("file.rs"), "fn x() {}\n").expect("write");
        Command::new("git")
            .args(["add", "."])
            .current_dir(root)
            .status()
            .expect("add");
        Command::new("git")
            .args(["commit", "-q", "-m", "init"])
            .current_dir(root)
            .status()
            .expect("commit");
        dir
    }

    #[test]
    fn creates_and_drops_worktree_cleanly() {
        let repo = init_repo();
        let parent = tempdir().expect("parent");
        {
            let _wt = TraceWorktree::create_under(repo.path(), parent.path()).expect("create");
            assert!(_wt.path.is_dir());
        }
        let entries: Vec<_> = std::fs::read_dir(parent.path())
            .expect("read")
            .filter_map(Result::ok)
            .collect();
        assert!(entries.is_empty(), "worktree should be cleaned up on drop");
    }
}
