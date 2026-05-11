use std::path::PathBuf;

pub fn pgs_path_for(project_root: &std::path::Path) -> PathBuf {
    let canonical = project_root
        .canonicalize()
        .unwrap_or_else(|_| project_root.to_path_buf());
    let project_hash = blake3::hash(canonical.as_os_str().as_encoded_bytes());
    let short: String = project_hash
        .as_bytes()
        .iter()
        .take(8)
        .map(|b| format!("{b:02x}"))
        .collect();
    let home = std::env::var_os("CODEX_HOME")
        .or_else(|| std::env::var_os("HOME").map(|h| {
            let mut p = PathBuf::from(h);
            p.push(".codex");
            p.into_os_string()
        }))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp/codex"));
    home.join("call-graph").join(short)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn different_projects_get_different_hashes() {
        let dir_a = tempfile::tempdir().expect("a");
        let dir_b = tempfile::tempdir().expect("b");
        assert_ne!(
            pgs_path_for(dir_a.path()),
            pgs_path_for(dir_b.path()),
            "different project roots must hash to different PGS directories"
        );
    }

    #[test]
    fn same_project_root_is_stable_across_calls() {
        let dir = tempfile::tempdir().expect("dir");
        assert_eq!(
            pgs_path_for(dir.path()),
            pgs_path_for(dir.path()),
            "same project root must produce the same PGS directory"
        );
    }

    #[test]
    fn pgs_path_always_lives_under_a_call_graph_subdir() {
        let dir = tempfile::tempdir().expect("dir");
        let path = pgs_path_for(dir.path());
        let path_str = path.display().to_string();
        assert!(
            path_str.contains("/call-graph/"),
            "PGS path must live under .../call-graph/<hash>, got {path_str}"
        );
    }
}
