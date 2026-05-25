// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! CI gate enforcing the cdylib-reachability invariant on the engine's
//! `Host*` Vulkan RHI primitives.
//!
//! The invariant: every constructor-class method (`new*`, `create*`,
//! `from_*`) on a `HostVulkan*` impl in the engine's RHI files must be
//! reachable from workspace plugin cdylibs without dispatching through
//! the `host_inner()` β-shape deref or the `host_callbacks()` guard.
//! Adding either inside a constructor body silently breaks the
//! cdylib's direct-call path documented on each type's
//! "Cdylib reachability" docstring (see
//! `docs/architecture/cdylib-reachability.md`).
//!
//! The check parses each target file with `syn` and walks the AST of
//! every constructor-class method's body, flagging any method-call
//! whose method is `host_inner` and any path expression whose last
//! segment is `host_inner` or `host_callbacks`. Comments and string
//! literals are skipped automatically by `syn`'s tokenization.
//!
//! See `docs/architecture/cdylib-reachability.md` for the decision
//! tree this check enforces.

use anyhow::{Context, Result};
use std::fs;
use std::path::{Path, PathBuf};
use syn::visit::Visit;
use walkdir::WalkDir;

/// Engine RHI directory whose `HostVulkan*` impls the check walks.
/// Every `.rs` file under this directory is parsed; only impls whose
/// type name starts with `HostVulkan` are visited. Adding a new
/// `Host*` type / new file under this tree is covered automatically
/// — the check tracks the directory, not a curated file list.
const TARGET_DIR: &str = "libs/streamlib-engine/src/vulkan/rhi";

/// Identifiers banned inside constructor-class function bodies.
const BANNED_IDENTS: &[&str] = &["host_inner", "host_callbacks"];

#[derive(Debug, PartialEq, Eq)]
pub struct Violation {
    pub file: PathBuf,
    pub impl_type: String,
    pub method: String,
    pub offending_ident: String,
    pub line: usize,
}

pub fn run(workspace_root: &Path) -> Result<()> {
    let violations = scan_workspace(workspace_root)?;
    if violations.is_empty() {
        println!(
            "✓ check-cdylib-reach: every Host* constructor in the engine RHI \
             stays clear of host_inner() / host_callbacks() — cdylib direct-call \
             path intact."
        );
        return Ok(());
    }

    eprintln!(
        "✗ check-cdylib-reach: {} violation(s) — Host* constructor reached for \
         a host-private guard, breaking the cdylib direct-call path:",
        violations.len()
    );
    for v in &violations {
        eprintln!(
            "  {}:{}: impl {} :: fn {} references `{}`",
            v.file.display(),
            v.line,
            v.impl_type,
            v.method,
            v.offending_ident,
        );
    }
    eprintln!(
        "\nFix:\n  \
         A `Host*::new*` / `create*` / `from_*` method body called `host_inner()` \
         or `host_callbacks()`. The cdylib direct-call path (see\n  \
         `docs/architecture/cdylib-reachability.md`) requires constructors to \
         touch only `pub` accessors on `HostVulkanDevice` plus `vulkanalia-vma`.\n  \
         Options:\n    \
           1. Re-route the body through a `pub` accessor (preferred).\n    \
           2. Promote the construction path to a FullAccess vtable slot \
              (route 1) — bump `GPU_CONTEXT_FULL_ACCESS_VTABLE_LAYOUT_VERSION`, \
              update the layout regression test, add a tier-1 wire-format test."
    );
    anyhow::bail!("check-cdylib-reach failed");
}

pub fn scan_workspace(workspace_root: &Path) -> Result<Vec<Violation>> {
    let mut all = Vec::new();
    let target_dir = workspace_root.join(TARGET_DIR);
    for entry in WalkDir::new(&target_dir)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        let abs = entry.path();
        if !entry.file_type().is_file() {
            continue;
        }
        if abs.extension().map(|e| e != "rs").unwrap_or(true) {
            continue;
        }
        let relpath = abs
            .strip_prefix(workspace_root)
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|_| abs.to_path_buf());
        let src = fs::read_to_string(abs)
            .with_context(|| format!("reading {}", abs.display()))?;
        let file = syn::parse_file(&src)
            .with_context(|| format!("parsing {}", abs.display()))?;
        let mut visitor = FileVisitor {
            file_path: relpath,
            current_impl_type: None,
            violations: &mut all,
        };
        visitor.visit_file(&file);
    }
    Ok(all)
}

fn is_constructor_name(name: &str) -> bool {
    name == "new"
        || name.starts_with("new_")
        || name == "create"
        || name.starts_with("create_")
        || name.starts_with("from_")
}

fn type_name(ty: &syn::Type) -> Option<String> {
    if let syn::Type::Path(tp) = ty {
        tp.path.segments.last().map(|s| s.ident.to_string())
    } else {
        None
    }
}

struct FileVisitor<'a> {
    file_path: PathBuf,
    current_impl_type: Option<String>,
    violations: &'a mut Vec<Violation>,
}

impl<'ast, 'a> Visit<'ast> for FileVisitor<'a> {
    fn visit_item_impl(&mut self, item: &'ast syn::ItemImpl) {
        // Only walk into `impl HostVulkanXxx` blocks. Other impls
        // (traits, non-Host types) pass through untouched.
        let ty = type_name(&item.self_ty).unwrap_or_default();
        let is_target = ty.starts_with("HostVulkan");
        if !is_target {
            return;
        }
        let prev = self.current_impl_type.replace(ty);
        syn::visit::visit_item_impl(self, item);
        self.current_impl_type = prev;
    }

    fn visit_impl_item_fn(&mut self, f: &'ast syn::ImplItemFn) {
        let Some(impl_ty) = self.current_impl_type.clone() else {
            return;
        };
        let name = f.sig.ident.to_string();
        if !is_constructor_name(&name) {
            return;
        }
        let mut body = BodyVisitor::default();
        body.visit_block(&f.block);
        for hit in body.hits {
            self.violations.push(Violation {
                file: self.file_path.clone(),
                impl_type: impl_ty.clone(),
                method: name.clone(),
                offending_ident: hit.ident,
                line: hit.line,
            });
        }
    }
}

struct Hit {
    ident: String,
    line: usize,
}

#[derive(Default)]
struct BodyVisitor {
    hits: Vec<Hit>,
}

impl<'ast> Visit<'ast> for BodyVisitor {
    fn visit_expr_method_call(&mut self, call: &'ast syn::ExprMethodCall) {
        let m = call.method.to_string();
        if BANNED_IDENTS.contains(&m.as_str()) {
            self.hits.push(Hit {
                ident: m,
                line: call.method.span().start().line,
            });
        }
        syn::visit::visit_expr_method_call(self, call);
    }

    fn visit_expr_path(&mut self, path: &'ast syn::ExprPath) {
        if let Some(seg) = path.path.segments.last() {
            let n = seg.ident.to_string();
            if BANNED_IDENTS.contains(&n.as_str()) {
                self.hits.push(Hit {
                    ident: n,
                    line: seg.ident.span().start().line,
                });
            }
        }
        syn::visit::visit_expr_path(self, path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use syn::visit::Visit;

    fn scan_source(src: &str) -> Vec<Violation> {
        let file = syn::parse_file(src).expect("parse");
        let mut violations = Vec::new();
        let mut visitor = FileVisitor {
            file_path: PathBuf::from("test.rs"),
            current_impl_type: None,
            violations: &mut violations,
        };
        visitor.visit_file(&file);
        violations
    }

    #[test]
    fn flags_host_inner_in_constructor() {
        let src = r#"
            impl HostVulkanBuffer {
                pub fn new(device: &u32) -> u32 {
                    self.host_inner();
                    0
                }
            }
        "#;
        let v = scan_source(src);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].impl_type, "HostVulkanBuffer");
        assert_eq!(v[0].method, "new");
        assert_eq!(v[0].offending_ident, "host_inner");
    }

    #[test]
    fn flags_host_callbacks_in_constructor() {
        let src = r#"
            impl HostVulkanTexture {
                pub fn new_render_target_dma_buf() -> u32 {
                    if host_callbacks().is_some() {
                        panic!("nope");
                    }
                    0
                }
            }
        "#;
        let v = scan_source(src);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].method, "new_render_target_dma_buf");
        assert_eq!(v[0].offending_ident, "host_callbacks");
    }

    #[test]
    fn flags_fully_qualified_host_callbacks() {
        let src = r#"
            impl HostVulkanTimelineSemaphore {
                pub fn new_exportable() -> u32 {
                    if crate::core::plugin::host_services::host_callbacks().is_some() {
                        return 0;
                    }
                    0
                }
            }
        "#;
        let v = scan_source(src);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].offending_ident, "host_callbacks");
    }

    #[test]
    fn ignores_non_constructor_methods() {
        // Non-constructor method names (like `wait`) are allowed to use
        // host_callbacks for vtable dispatch — that's the documented
        // shape for runtime methods.
        let src = r#"
            impl HostVulkanTimelineSemaphore {
                pub fn wait(&self) -> u32 {
                    if host_callbacks().is_some() {
                        return self.wait_via_vtable();
                    }
                    0
                }
            }
        "#;
        let v = scan_source(src);
        assert!(v.is_empty(), "wait() is not a constructor; allowed");
    }

    #[test]
    fn ignores_non_host_impls() {
        // The check only walks impls of `HostVulkanXxx` types. Other
        // types in the same file get to use host_inner / host_callbacks
        // freely.
        let src = r#"
            impl SomeOtherType {
                pub fn new() -> u32 {
                    self.host_inner();
                    0
                }
            }
        "#;
        let v = scan_source(src);
        assert!(v.is_empty(), "non-Host impl ignored");
    }

    #[test]
    fn ignores_docstring_mentions() {
        // The whole point of the AST walk: docstrings and comments
        // never become Expr nodes.
        let src = r#"
            impl HostVulkanBuffer {
                /// This constructor does NOT call host_inner() or
                /// host_callbacks() — see docs.
                pub fn new() -> u32 {
                    0
                }
            }
        "#;
        let v = scan_source(src);
        assert!(v.is_empty(), "docstring mentions are not call sites");
    }

    #[test]
    fn ignores_string_literal_mentions() {
        let src = r#"
            impl HostVulkanBuffer {
                pub fn new() -> Result<u32, String> {
                    Err("host_inner failed (this is just a message)".into())
                }
            }
        "#;
        let v = scan_source(src);
        assert!(v.is_empty(), "string literal mentions are not call sites");
    }

    #[test]
    fn flags_create_helper() {
        // Internal helpers like `HostVulkanTimelineSemaphore::create`
        // are part of the constructor chain — `new` / `new_exportable`
        // both call them — so they're covered too.
        let src = r#"
            impl HostVulkanTimelineSemaphore {
                fn create() -> u32 {
                    self.host_inner();
                    0
                }
            }
        "#;
        let v = scan_source(src);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].method, "create");
    }

    #[test]
    fn flags_from_helper() {
        let src = r#"
            impl HostVulkanBuffer {
                pub fn from_dma_buf_fd() -> u32 {
                    let x = host_callbacks();
                    0
                }
            }
        "#;
        let v = scan_source(src);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].method, "from_dma_buf_fd");
    }
}
