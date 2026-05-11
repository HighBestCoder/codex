mod map;
mod pgs_path;
mod server;
mod why;

pub use map::{MapError, MapRequest, MapResponse, run_map};
pub use pgs_path::pgs_path_for;
pub use server::{CallGraphMcpServer, run_server, run_stdio_server};
pub use why::{EdgeRef, WhyError, WhyMatch, WhyRequest, WhyResponse, run_why};

#[cfg(test)]
mod e2e_tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    fn write_rust_file(root: &std::path::Path, rel: &str, body: &str) {
        let path = root.join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create dir");
        }
        fs::write(&path, body).expect("write");
    }

    fn project_with_two_rust_files() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        write_rust_file(
            dir.path(),
            "src/lib.rs",
            "pub fn double(x: i32) -> i32 { x + x }\n\
             pub fn quadruple(x: i32) -> i32 { double(double(x)) }\n",
        );
        write_rust_file(
            dir.path(),
            "src/main.rs",
            "fn main() { let _ = quadruple(3); }\n\
             fn quadruple(x: i32) -> i32 { x * 4 }\n",
        );
        dir
    }

    #[test]
    fn run_map_then_run_why_returns_callers_and_callees() {
        let dir = project_with_two_rust_files();
        let project_root = dir
            .path()
            .canonicalize()
            .expect("canonicalize tempdir")
            .display()
            .to_string();

        let map_resp = run_map(&MapRequest {
            project_root: project_root.clone(),
        })
        .expect("map");
        assert!(map_resp.nodes_added >= 4, "expected >=4 nodes, got {map_resp:?}");
        assert!(
            map_resp.files_indexed >= 2,
            "expected >=2 files indexed, got {map_resp:?}"
        );

        let why_resp = run_why(&WhyRequest {
            project_root,
            symbol: "double".into(),
            max_callers: Some(50),
            max_callees: Some(50),
        })
        .expect("why");

        assert!(
            !why_resp.matches.is_empty(),
            "expected at least one match for 'double'"
        );
        let first = &why_resp.matches[0];
        assert_eq!(first.symbol, "double");
        let has_caller_named_quadruple = first
            .callers
            .iter()
            .any(|c| c.symbol == "quadruple");
        assert!(
            has_caller_named_quadruple,
            "expected 'quadruple' as a caller of 'double', got {:?}",
            first.callers
        );
    }

    #[test]
    fn run_why_returns_pgs_missing_when_map_never_ran() {
        let dir = tempfile::tempdir().expect("tempdir");
        let project_root = dir.path().canonicalize().expect("canon").display().to_string();
        let result = run_why(&WhyRequest {
            project_root,
            symbol: "anything".into(),
            max_callers: None,
            max_callees: None,
        });
        match result {
            Err(WhyError::PgsMissing { .. }) => {}
            other => panic!("expected PgsMissing, got {other:?}"),
        }
    }

    #[test]
    fn run_map_rejects_relative_project_root() {
        let result = run_map(&MapRequest {
            project_root: "relative/path".into(),
        });
        match result {
            Err(MapError::NotAbsolute(p)) => {
                assert_eq!(p, PathBuf::from("relative/path"));
            }
            other => panic!("expected NotAbsolute, got {other:?}"),
        }
    }
}
