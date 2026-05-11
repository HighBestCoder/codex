use std::path::{Path, PathBuf};

use codex_call_graph_store::{EdgeSource, ParsedFile, RawCallEdge, RawFnNode, RawTarget};
use rayon::prelude::*;
use syn::spanned::Spanned;
use syn::visit::Visit;
use walkdir::WalkDir;

use crate::error::PerceptionError;

#[derive(Debug, Clone, Default)]
pub struct ProjectParse {
    pub files: Vec<ParsedFile>,
    pub failures: Vec<(PathBuf, String)>,
    pub total_files_seen: usize,
}

pub fn parse_file(path: &Path) -> Result<ParsedFile, PerceptionError> {
    let src = std::fs::read_to_string(path).map_err(|err| PerceptionError::Io {
        path: path.to_path_buf(),
        source: err,
    })?;
    let file_ast = syn::parse_file(&src).map_err(|err| PerceptionError::Syn {
        path: path.to_path_buf(),
        source: err,
    })?;
    let fingerprint = blake3_truncated(src.as_bytes());
    let mut visitor = FnVisitor {
        file: path.to_path_buf(),
        fn_stack: Vec::new(),
        impl_owner: None,
        nodes: Vec::new(),
        edges: Vec::new(),
        fingerprint,
    };
    visitor.visit_file(&file_ast);
    Ok(ParsedFile {
        file: path.to_path_buf(),
        nodes: visitor.nodes,
        edges: visitor.edges,
    })
}

pub fn parse_project(root: &Path) -> Result<ProjectParse, PerceptionError> {
    let files = collect_rust_files(root);
    let total_files_seen = files.len();
    let parsed: Vec<Result<ParsedFile, (PathBuf, String)>> = files
        .par_iter()
        .map(|path| match parse_file(path) {
            Ok(parsed) => Ok(parsed),
            Err(err) => Err((path.clone(), err.to_string())),
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

fn collect_rust_files(root: &Path) -> Vec<PathBuf> {
    WalkDir::new(root)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|e| {
            e.file_type().is_file()
                && e.path().extension().is_some_and(|ext| ext == "rs")
                && !e.path().components().any(|comp| {
                    matches!(comp.as_os_str().to_str(), Some("target") | Some(".git"))
                })
        })
        .map(|e| e.into_path())
        .collect()
}

fn blake3_truncated(bytes: &[u8]) -> [u8; 16] {
    let hash = blake3::hash(bytes);
    let full = hash.as_bytes();
    let mut out = [0u8; 16];
    out.copy_from_slice(&full[..16]);
    out
}

struct FnVisitor {
    #[allow(dead_code)]
    file: PathBuf,
    fn_stack: Vec<String>,
    impl_owner: Option<String>,
    nodes: Vec<RawFnNode>,
    edges: Vec<RawCallEdge>,
    fingerprint: [u8; 16],
}

impl FnVisitor {
    fn record_fn(&mut self, sig: &syn::Signature, block: &syn::Block) {
        let name = sig.ident.to_string();
        let line = sig.ident.span().start().line as u32;
        let loc = block_loc(block);
        let is_async = sig.asyncness.is_some();
        let is_method = self.impl_owner.is_some();

        self.nodes.push(RawFnNode {
            simple_name: name.clone(),
            line,
            is_async,
            is_method,
            loc,
            fingerprint: self.fingerprint,
        });

        self.fn_stack.push(name);
        syn::visit::visit_block(self, block);
        self.fn_stack.pop();
    }
}

impl<'ast> Visit<'ast> for FnVisitor {
    fn visit_item_mod(&mut self, m: &'ast syn::ItemMod) {
        if let Some((_, items)) = &m.content {
            for item in items {
                self.visit_item(item);
            }
        }
    }

    fn visit_item_impl(&mut self, im: &'ast syn::ItemImpl) {
        let owner = match &*im.self_ty {
            syn::Type::Path(p) => p
                .path
                .segments
                .last()
                .map(|s| s.ident.to_string())
                .unwrap_or_else(|| "_".into()),
            _ => "_".into(),
        };
        let prev = self.impl_owner.replace(owner);
        for item in &im.items {
            if let syn::ImplItem::Fn(f) = item {
                self.record_fn(&f.sig, &f.block);
            }
        }
        self.impl_owner = prev;
    }

    fn visit_item_fn(&mut self, f: &'ast syn::ItemFn) {
        self.record_fn(&f.sig, &f.block);
    }

    fn visit_expr_call(&mut self, c: &'ast syn::ExprCall) {
        if let Some(caller) = self.fn_stack.last().cloned() {
            if let syn::Expr::Path(p) = &*c.func {
                if let Some(last) = p.path.segments.last() {
                    let callee = last.ident.to_string();
                    let line = last.ident.span().start().line as u32;
                    self.edges.push(RawCallEdge {
                        caller_simple_name: caller,
                        caller_line: 0,
                        line,
                        source: EdgeSource::SimpleName,
                        confidence: EdgeSource::SimpleName.default_confidence(),
                        target: RawTarget::Simple {
                            callee_simple_name: callee,
                        },
                    });
                }
            }
        }
        syn::visit::visit_expr_call(self, c);
    }

    fn visit_expr_method_call(&mut self, c: &'ast syn::ExprMethodCall) {
        if let Some(caller) = self.fn_stack.last().cloned() {
            let callee = c.method.to_string();
            let line = c.method.span().start().line as u32;
            self.edges.push(RawCallEdge {
                caller_simple_name: caller,
                caller_line: 0,
                line,
                source: EdgeSource::SimpleName,
                confidence: EdgeSource::SimpleName.default_confidence(),
                target: RawTarget::Simple {
                    callee_simple_name: callee,
                },
            });
        }
        syn::visit::visit_expr_method_call(self, c);
    }
}

fn block_loc(block: &syn::Block) -> u32 {
    let span = block.span();
    let start = span.start().line as u32;
    let end = span.end().line as u32;
    end.saturating_sub(start).max(1)
}
