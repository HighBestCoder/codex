use std::path::{Path, PathBuf};

use git2::{BlameOptions, Repository};

#[derive(Debug, Clone, Default)]
pub struct NodeHistory {
    pub first_introduced_commit: Option<String>,
    pub first_introduced_author: Option<String>,
    pub first_introduced_unix_ts: Option<i64>,
    pub recent_revert_count: u32,
}

#[derive(Debug, thiserror::Error)]
pub enum HistoryError {
    #[error("git error: {0}")]
    Git(#[from] git2::Error),
    #[error("file is outside the repository working tree: {0}")]
    NotInRepo(PathBuf),
}

pub fn open_repo(start: &Path) -> Option<Repository> {
    Repository::discover(start).ok()
}

pub fn blame_history(
    repo: &Repository,
    file: &Path,
    line: u32,
) -> Result<NodeHistory, HistoryError> {
    let workdir = repo
        .workdir()
        .ok_or_else(|| HistoryError::NotInRepo(file.to_path_buf()))?;
    let absolute = if file.is_absolute() {
        file.to_path_buf()
    } else {
        workdir.join(file)
    };
    let canonical = absolute
        .canonicalize()
        .unwrap_or_else(|_| absolute.clone());
    let workdir_canon = workdir
        .canonicalize()
        .unwrap_or_else(|_| workdir.to_path_buf());
    let relative = canonical
        .strip_prefix(&workdir_canon)
        .map(Path::to_path_buf)
        .map_err(|_| HistoryError::NotInRepo(file.to_path_buf()))?;

    let mut options = BlameOptions::new();
    if line > 0 {
        let line = line as usize;
        options.min_line(line).max_line(line);
    }
    let blame = repo.blame_file(&relative, Some(&mut options))?;
    let Some(hunk) = blame.iter().next() else {
        return Ok(NodeHistory::default());
    };
    let oid = hunk.final_commit_id();
    let commit = repo.find_commit(oid)?;
    let signature = commit.author();
    let history = NodeHistory {
        first_introduced_commit: Some(short_oid(&oid)),
        first_introduced_author: signature
            .name()
            .map(str::to_string)
            .or_else(|| signature.email().map(str::to_string)),
        first_introduced_unix_ts: Some(commit.time().seconds()),
        recent_revert_count: 0,
    };
    Ok(history)
}

pub fn count_recent_reverts(
    repo: &Repository,
    file: &Path,
    window_secs: i64,
) -> Result<u32, HistoryError> {
    let workdir = repo
        .workdir()
        .ok_or_else(|| HistoryError::NotInRepo(file.to_path_buf()))?;
    let absolute = if file.is_absolute() {
        file.to_path_buf()
    } else {
        workdir.join(file)
    };
    let canonical = absolute
        .canonicalize()
        .unwrap_or_else(|_| absolute.clone());
    let workdir_canon = workdir
        .canonicalize()
        .unwrap_or_else(|_| workdir.to_path_buf());
    let relative = canonical
        .strip_prefix(&workdir_canon)
        .map(Path::to_path_buf)
        .map_err(|_| HistoryError::NotInRepo(file.to_path_buf()))?;

    let mut walker = repo.revwalk()?;
    walker.push_head()?;
    walker.set_sorting(git2::Sort::TIME)?;
    let cutoff = chrono_now()? - window_secs;
    let mut count = 0u32;
    for oid in walker {
        let oid = oid?;
        let commit = repo.find_commit(oid)?;
        if commit.time().seconds() < cutoff {
            break;
        }
        let message = commit.message().unwrap_or("");
        let mentions_revert =
            message.starts_with("Revert ") || message.contains("\nRevert ") || message.contains("revert:");
        if !mentions_revert {
            continue;
        }
        let parents = commit.parents().collect::<Vec<_>>();
        let parent_tree = parents.first().map(|p| p.tree()).transpose()?;
        let tree = commit.tree()?;
        let diff = repo.diff_tree_to_tree(parent_tree.as_ref(), Some(&tree), None)?;
        let mut touches = false;
        diff.foreach(
            &mut |delta, _| {
                if delta
                    .new_file()
                    .path()
                    .map(|p| p == relative)
                    .unwrap_or(false)
                {
                    touches = true;
                }
                true
            },
            None,
            None,
            None,
        )?;
        if touches {
            count += 1;
        }
    }
    Ok(count)
}

fn short_oid(oid: &git2::Oid) -> String {
    let bytes = oid.as_bytes();
    let mut s = String::with_capacity(14);
    for b in bytes.iter().take(7) {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

fn chrono_now() -> Result<i64, HistoryError> {
    Ok(std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    use tempfile::tempdir;

    fn run(cmd: &mut Command) -> std::process::Output {
        let out = cmd.output().expect("git command");
        if !out.status.success() {
            panic!(
                "git failed: {:?}\nstdout: {}\nstderr: {}",
                cmd,
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
        }
        out
    }

    fn init_repo_with_one_commit() -> (tempfile::TempDir, PathBuf) {
        let dir = tempdir().expect("tempdir");
        let root = dir.path().to_path_buf();
        run(Command::new("git")
            .arg("init")
            .arg("-q")
            .arg("--initial-branch=main")
            .current_dir(&root));
        run(Command::new("git")
            .args(["config", "user.email", "test@example.invalid"])
            .current_dir(&root));
        run(Command::new("git")
            .args(["config", "user.name", "Tester"])
            .current_dir(&root));
        let file_path = root.join("lib.rs");
        std::fs::write(&file_path, "fn one() {}\nfn two() {}\n").expect("write");
        run(Command::new("git").args(["add", "."]).current_dir(&root));
        run(Command::new("git")
            .args(["commit", "-q", "-m", "initial"])
            .current_dir(&root));
        (dir, file_path)
    }

    #[test]
    fn blame_history_reports_initial_commit() {
        let (dir, file) = init_repo_with_one_commit();
        let repo = Repository::open(dir.path()).expect("open repo");
        let history = blame_history(&repo, &file, 1).expect("blame");
        assert!(history.first_introduced_commit.is_some());
        assert_eq!(history.first_introduced_author.as_deref(), Some("Tester"));
        assert!(history.first_introduced_unix_ts.unwrap_or(0) > 0);
    }

    #[test]
    fn count_recent_reverts_returns_zero_with_no_reverts() {
        let (dir, file) = init_repo_with_one_commit();
        let repo = Repository::open(dir.path()).expect("open repo");
        let count = count_recent_reverts(&repo, &file, 60 * 60 * 24 * 365).expect("count");
        assert_eq!(count, 0);
    }
}
