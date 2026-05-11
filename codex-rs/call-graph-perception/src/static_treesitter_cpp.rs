use std::path::{Path, PathBuf};

use codex_call_graph_store::{EdgeSource, ParsedFile, RawCallEdge, RawFnNode, RawTarget};
use rayon::prelude::*;
use tree_sitter::{Node, Parser, Tree};
use walkdir::WalkDir;

use crate::error::PerceptionError;
use crate::static_simple::ProjectParse;

const CPP_EXTENSIONS: &[&str] = &["cpp", "cc", "cxx", "c++", "C", "hpp", "hh", "hxx", "h++"];
const C_EXTENSIONS: &[&str] = &["c", "h"];

#[derive(Debug, Clone, Copy)]
pub enum CppDialect {
    Cpp,
    C,
}

pub fn parse_cpp_file(path: &Path) -> Result<ParsedFile, PerceptionError> {
    let dialect = pick_dialect(path);
    let src = std::fs::read_to_string(path).map_err(|err| PerceptionError::Io {
        path: path.to_path_buf(),
        source: err,
    })?;
    let mut parser = Parser::new();
    let language: tree_sitter::Language = match dialect {
        CppDialect::Cpp => tree_sitter_cpp::LANGUAGE.into(),
        CppDialect::C => tree_sitter_c::LANGUAGE.into(),
    };
    parser.set_language(&language).map_err(|err| {
        PerceptionError::ScipParse(format!("tree-sitter set_language failed: {err}"))
    })?;
    let tree = parser
        .parse(&src, None)
        .ok_or_else(|| PerceptionError::ScipParse("tree-sitter returned no tree".into()))?;
    Ok(extract_parsed_file(path, &src, &tree))
}

fn extract_parsed_file(path: &Path, src: &str, tree: &Tree) -> ParsedFile {
    let bytes = src.as_bytes();
    let mut nodes: Vec<RawFnNode> = Vec::new();
    let mut edges: Vec<RawCallEdge> = Vec::new();
    let mut fn_stack: Vec<String> = Vec::new();
    walk_node(tree.root_node(), bytes, &mut fn_stack, &mut nodes, &mut edges);
    ParsedFile {
        file: path.to_path_buf(),
        nodes,
        edges,
    }
}

fn walk_node<'a>(
    node: Node<'a>,
    src: &[u8],
    fn_stack: &mut Vec<String>,
    nodes: &mut Vec<RawFnNode>,
    edges: &mut Vec<RawCallEdge>,
) {
    match node.kind() {
        "function_definition" => {
            if let Some(decl) = node.child_by_field_name("declarator") {
                if let Some(name) = function_name_from_declarator(decl, src) {
                    let line = (node.start_position().row + 1) as u32;
                    let loc = (node
                        .end_position()
                        .row
                        .saturating_sub(node.start_position().row)
                        .max(1)) as u32;
                    nodes.push(RawFnNode {
                        simple_name: name.clone(),
                        line,
                        is_async: false,
                        is_method: name.contains("::"),
                        loc,
                        fingerprint: blake3_truncated(node_text(node, src).as_bytes()),
                    });
                    fn_stack.push(name);
                    walk_children(node, src, fn_stack, nodes, edges);
                    fn_stack.pop();
                    return;
                }
            }
        }
        "call_expression" => {
            if let Some(callee_name) = call_callee_name(node, src) {
                if let Some(caller) = fn_stack.last().cloned() {
                    let line = (node.start_position().row + 1) as u32;
                    edges.push(RawCallEdge {
                        caller_simple_name: caller,
                        caller_line: 0,
                        line,
                        source: EdgeSource::SimpleName,
                        confidence: EdgeSource::SimpleName.default_confidence(),
                        target: RawTarget::Simple {
                            callee_simple_name: callee_name,
                        },
                    });
                }
            }
        }
        _ => {}
    }
    walk_children(node, src, fn_stack, nodes, edges);
}

fn walk_children<'a>(
    node: Node<'a>,
    src: &[u8],
    fn_stack: &mut Vec<String>,
    nodes: &mut Vec<RawFnNode>,
    edges: &mut Vec<RawCallEdge>,
) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk_node(child, src, fn_stack, nodes, edges);
    }
}

fn function_name_from_declarator(node: Node<'_>, src: &[u8]) -> Option<String> {
    let mut current = node;
    loop {
        match current.kind() {
            "function_declarator" => {
                let decl = current.child_by_field_name("declarator")?;
                return identifier_text(decl, src);
            }
            "pointer_declarator" | "reference_declarator" => {
                if let Some(inner) = current.child_by_field_name("declarator") {
                    current = inner;
                    continue;
                }
                return None;
            }
            _ => return None,
        }
    }
}

fn identifier_text(node: Node<'_>, src: &[u8]) -> Option<String> {
    match node.kind() {
        "identifier" | "field_identifier" | "operator_name" | "destructor_name"
        | "type_identifier" => Some(node_text(node, src)),
        "qualified_identifier" => {
            if let Some(inner) = node.child_by_field_name("name") {
                identifier_text(inner, src)
            } else {
                Some(node_text(node, src))
            }
        }
        "template_function" => {
            if let Some(inner) = node.child_by_field_name("name") {
                identifier_text(inner, src)
            } else {
                None
            }
        }
        _ => None,
    }
}

fn call_callee_name(node: Node<'_>, src: &[u8]) -> Option<String> {
    let func = node.child_by_field_name("function")?;
    extract_call_name(func, src)
}

fn extract_call_name(node: Node<'_>, src: &[u8]) -> Option<String> {
    match node.kind() {
        "identifier" | "field_identifier" => Some(node_text(node, src)),
        "qualified_identifier" => node
            .child_by_field_name("name")
            .and_then(|n| extract_call_name(n, src)),
        "field_expression" => node
            .child_by_field_name("field")
            .and_then(|n| extract_call_name(n, src)),
        "template_function" => node
            .child_by_field_name("name")
            .and_then(|n| extract_call_name(n, src)),
        "parenthesized_expression" => {
            for child in node.named_children(&mut node.walk()) {
                if let Some(name) = extract_call_name(child, src) {
                    return Some(name);
                }
            }
            None
        }
        _ => None,
    }
}

fn node_text(node: Node<'_>, src: &[u8]) -> String {
    src[node.byte_range()]
        .iter()
        .map(|&b| b as char)
        .collect::<String>()
}

fn blake3_truncated(bytes: &[u8]) -> [u8; 16] {
    let hash = blake3::hash(bytes);
    let mut out = [0u8; 16];
    out.copy_from_slice(&hash.as_bytes()[..16]);
    out
}

fn pick_dialect(path: &Path) -> CppDialect {
    let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
        return CppDialect::Cpp;
    };
    if CPP_EXTENSIONS.iter().any(|x| x.eq_ignore_ascii_case(ext)) {
        return CppDialect::Cpp;
    }
    if C_EXTENSIONS.iter().any(|x| x.eq_ignore_ascii_case(ext)) {
        return CppDialect::C;
    }
    CppDialect::Cpp
}

pub fn parse_cpp_project(root: &Path) -> Result<ProjectParse, PerceptionError> {
    let mut all_extensions: Vec<&str> = Vec::with_capacity(CPP_EXTENSIONS.len() + C_EXTENSIONS.len());
    all_extensions.extend_from_slice(CPP_EXTENSIONS);
    all_extensions.extend_from_slice(C_EXTENSIONS);
    let files: Vec<PathBuf> = WalkDir::new(root)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|e| {
            e.file_type().is_file()
                && e.path()
                    .extension()
                    .and_then(|x| x.to_str())
                    .is_some_and(|x| all_extensions.iter().any(|e| e.eq_ignore_ascii_case(x)))
                && !e
                    .path()
                    .components()
                    .any(|c| matches!(c.as_os_str().to_str(), Some("build") | Some(".git") | Some("target")))
        })
        .map(|e| e.into_path())
        .collect();
    let total_files_seen = files.len();
    let parsed: Vec<Result<ParsedFile, (PathBuf, String)>> = files
        .par_iter()
        .map(|p| match parse_cpp_file(p) {
            Ok(parsed) => Ok(parsed),
            Err(err) => Err((p.clone(), err.to_string())),
        })
        .collect();
    let mut project = ProjectParse {
        total_files_seen,
        ..ProjectParse::default()
    };
    for outcome in parsed {
        match outcome {
            Ok(parsed) => project.files.push(parsed),
            Err(failure) => project.failures.push(failure),
        }
    }
    Ok(project)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn write(dir: &Path, name: &str, body: &str) -> PathBuf {
        let path = dir.join(name);
        fs::write(&path, body).expect("write");
        path
    }

    #[test]
    fn parses_simple_cpp_function_definitions_and_calls() {
        let dir = tempdir().expect("tempdir");
        let path = write(
            dir.path(),
            "lib.cpp",
            "int add(int a, int b) { return a + b; }\n\
             int double_it(int x) { return add(x, x); }\n\
             int quadruple(int x) { int d = double_it(x); return double_it(d); }\n",
        );
        let parsed = parse_cpp_file(&path).expect("parse");
        let names: Vec<&str> = parsed.nodes.iter().map(|n| n.simple_name.as_str()).collect();
        assert_eq!(names, vec!["add", "double_it", "quadruple"]);
        let edges: Vec<(String, String)> = parsed
            .edges
            .iter()
            .map(|e| match &e.target {
                RawTarget::Simple { callee_simple_name } => {
                    (e.caller_simple_name.clone(), callee_simple_name.clone())
                }
                _ => panic!("expected simple-name target"),
            })
            .collect();
        assert!(edges.contains(&("double_it".into(), "add".into())));
        assert_eq!(
            edges
                .iter()
                .filter(|(c, t)| c == "quadruple" && t == "double_it")
                .count(),
            2
        );
    }

    #[test]
    fn parses_class_method_definitions() {
        let dir = tempdir().expect("tempdir");
        let path = write(
            dir.path(),
            "thing.cpp",
            "class Thing {\npublic:\n  int run(int x) { return helper(x); }\n  int helper(int x) { return x; }\n};\n",
        );
        let parsed = parse_cpp_file(&path).expect("parse");
        let names: Vec<&str> = parsed.nodes.iter().map(|n| n.simple_name.as_str()).collect();
        assert!(names.contains(&"run"));
        assert!(names.contains(&"helper"));
    }

    #[test]
    fn parses_plain_c_file_with_dot_c_extension() {
        let dir = tempdir().expect("tempdir");
        let path = write(
            dir.path(),
            "ops.c",
            "int add(int a, int b) { return a + b; }\nint use(void) { return add(1, 2); }\n",
        );
        let parsed = parse_cpp_file(&path).expect("parse");
        let names: Vec<&str> = parsed.nodes.iter().map(|n| n.simple_name.as_str()).collect();
        assert!(names.contains(&"add"));
        assert!(names.contains(&"use"));
    }

    #[test]
    fn parse_cpp_project_walks_nested_directories() {
        let dir = tempdir().expect("tempdir");
        write(dir.path(), "a.cpp", "int a() { return 1; }\n");
        let nested = dir.path().join("nested");
        fs::create_dir_all(&nested).expect("nested");
        write(&nested, "b.cpp", "int b() { return 2; }\n");
        let project = parse_cpp_project(dir.path()).expect("project");
        assert_eq!(project.total_files_seen, 2);
        assert_eq!(project.files.len(), 2);
    }
}
