use std::path::{Path, PathBuf};

use rayon::prelude::*;
use syn::visit_mut::{self, VisitMut};
use syn::{parse_quote, parse_str, Attribute, ImplItem, ItemFn};
use walkdir::WalkDir;

#[derive(Debug, Clone, Default)]
pub struct InjectStats {
    pub files_visited: usize,
    pub files_modified: usize,
    pub free_fns_injected: usize,
    pub impl_fns_injected: usize,
    pub skipped_const: usize,
    pub skipped_extern: usize,
    pub skipped_already: usize,
    pub failures: Vec<(PathBuf, String)>,
}

#[derive(Debug, Clone)]
pub struct InjectOptions {
    pub skip_paths: Vec<PathBuf>,
    pub max_threads: Option<usize>,
}

impl Default for InjectOptions {
    fn default() -> Self {
        Self {
            skip_paths: Vec::new(),
            max_threads: None,
        }
    }
}

pub fn inject_workspace(workspace_root: &Path, options: &InjectOptions) -> InjectStats {
    let files: Vec<PathBuf> = WalkDir::new(workspace_root)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|e| {
            e.file_type().is_file()
                && e.path().extension().is_some_and(|ext| ext == "rs")
                && !e.path().components().any(|c| {
                    matches!(c.as_os_str().to_str(), Some("target") | Some(".git"))
                })
                && !options.skip_paths.iter().any(|sk| e.path().starts_with(sk))
        })
        .map(|e| e.into_path())
        .collect();

    let outcomes: Vec<Result<(PathBuf, FileInjectResult), (PathBuf, String)>> = files
        .par_iter()
        .map(|path| match inject_file(path) {
            Ok(result) => Ok((path.clone(), result)),
            Err(err) => Err((path.clone(), err.to_string())),
        })
        .collect();

    let mut stats = InjectStats {
        files_visited: files.len(),
        ..InjectStats::default()
    };
    for outcome in outcomes {
        match outcome {
            Ok((_path, result)) => {
                if result.modified {
                    stats.files_modified += 1;
                }
                stats.free_fns_injected += result.free_fns;
                stats.impl_fns_injected += result.impl_fns;
                stats.skipped_const += result.skipped_const;
                stats.skipped_extern += result.skipped_extern;
                stats.skipped_already += result.skipped_already;
            }
            Err(failure) => stats.failures.push(failure),
        }
    }
    stats
}

#[derive(Debug, Clone, Default)]
pub struct FileInjectResult {
    pub modified: bool,
    pub free_fns: usize,
    pub impl_fns: usize,
    pub skipped_const: usize,
    pub skipped_extern: usize,
    pub skipped_already: usize,
}

#[derive(Debug, thiserror::Error)]
pub enum InjectError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("syn parse error: {0}")]
    Syn(#[from] syn::Error),
}

pub fn inject_file(path: &Path) -> Result<FileInjectResult, InjectError> {
    let src = std::fs::read_to_string(path)?;
    let (out, stats) = inject_source(&src)?;
    let modified = out != src;
    if modified {
        std::fs::write(path, &out)?;
    }
    Ok(FileInjectResult {
        modified,
        free_fns: stats.free_fns,
        impl_fns: stats.impl_fns,
        skipped_const: stats.skipped_const,
        skipped_extern: stats.skipped_extern,
        skipped_already: stats.skipped_already,
    })
}

#[derive(Default)]
struct InjectorVisitor {
    free_fns: usize,
    impl_fns: usize,
    skipped_const: usize,
    skipped_extern: usize,
    skipped_already: usize,
}

impl InjectorVisitor {
    fn has_instrument(attrs: &[Attribute]) -> bool {
        attrs.iter().any(|attr| {
            attr.path()
                .segments
                .last()
                .map(|seg| seg.ident == "instrument")
                .unwrap_or(false)
        })
    }
}

impl VisitMut for InjectorVisitor {
    fn visit_item_fn_mut(&mut self, node: &mut ItemFn) {
        if node.sig.constness.is_some() {
            self.skipped_const += 1;
        } else if Self::has_instrument(&node.attrs) {
            self.skipped_already += 1;
        } else {
            let attr: Attribute = parse_quote!(#[tracing::instrument]);
            node.attrs.push(attr);
            self.free_fns += 1;
        }
        visit_mut::visit_item_fn_mut(self, node);
    }

    fn visit_item_impl_mut(&mut self, node: &mut syn::ItemImpl) {
        for item in &mut node.items {
            if let ImplItem::Fn(f) = item {
                if f.sig.constness.is_some() {
                    self.skipped_const += 1;
                    continue;
                }
                if Self::has_instrument(&f.attrs) {
                    self.skipped_already += 1;
                    continue;
                }
                let has_self = f
                    .sig
                    .inputs
                    .iter()
                    .any(|a| matches!(a, syn::FnArg::Receiver(_)));
                let attr: Attribute = if has_self {
                    parse_quote!(#[tracing::instrument(skip(self))])
                } else {
                    parse_quote!(#[tracing::instrument])
                };
                f.attrs.push(attr);
                self.impl_fns += 1;
            }
        }
        visit_mut::visit_item_impl_mut(self, node);
    }

    fn visit_item_foreign_mod_mut(&mut self, node: &mut syn::ItemForeignMod) {
        self.skipped_extern += node.items.len();
    }
}

pub fn inject_source(src: &str) -> Result<(String, InjectorStats), InjectError> {
    let mut file = parse_str::<syn::File>(src)?;
    let mut visitor = InjectorVisitor::default();
    visitor.visit_file_mut(&mut file);
    let out = prettyplease::unparse(&file);
    Ok((
        out,
        InjectorStats {
            free_fns: visitor.free_fns,
            impl_fns: visitor.impl_fns,
            skipped_const: visitor.skipped_const,
            skipped_extern: visitor.skipped_extern,
            skipped_already: visitor.skipped_already,
        },
    ))
}

#[derive(Debug, Clone, Default)]
pub struct InjectorStats {
    pub free_fns: usize,
    pub impl_fns: usize,
    pub skipped_const: usize,
    pub skipped_extern: usize,
    pub skipped_already: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn injects_free_fn_with_instrument_attribute() {
        let src = "fn add(a: i64, b: i64) -> i64 { a + b }\n";
        let (out, stats) = inject_source(src).expect("inject");
        assert_eq!(stats.free_fns, 1);
        assert!(out.contains("#[tracing::instrument]"));
    }

    #[test]
    fn skips_const_fn() {
        let src = "const fn add(a: i64) -> i64 { a + 1 }\n";
        let (out, stats) = inject_source(src).expect("inject");
        assert_eq!(stats.skipped_const, 1);
        assert!(!out.contains("instrument"));
    }

    #[test]
    fn impl_with_self_uses_skip_self() {
        let src = "struct S; impl S { fn ping(&self) -> i32 { 1 } }\n";
        let (out, stats) = inject_source(src).expect("inject");
        assert_eq!(stats.impl_fns, 1);
        assert!(out.contains("skip(self)"));
    }

    #[test]
    fn already_instrumented_fn_is_idempotent() {
        let src = "#[tracing::instrument] fn already() {}\n";
        let (_out, stats) = inject_source(src).expect("inject");
        assert_eq!(stats.skipped_already, 1);
        assert_eq!(stats.free_fns, 0);
    }
}
