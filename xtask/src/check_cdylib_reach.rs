// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! CI gate enforcing the cdylib-reachability invariant on the engine's
//! `Host*` Vulkan RHI primitives plus every workspace cdylib's
//! dispatch paths.
//!
//! Two scans run, each catching one direction of the same boundary
//! violation:
//!
//! 1. **Engine-side (`scan_workspace`)**: every constructor-class
//!    method (`new*`, `create*`, `from_*`) on a `HostVulkan*` impl in
//!    the engine's RHI files must stay clear of `host_inner()` and
//!    `host_callbacks()`. Adding either inside a constructor body
//!    silently breaks the cdylib direct-call path the
//!    "Cdylib reachability" docstrings on each type document.
//!
//! 2. **Cdylib-side (`scan_cdylib_dispatch_paths`)**: every
//!    workspace crate with `crate-type` containing `"cdylib"` (every
//!    `packages/*` plugin, every `examples/**/plugin` plugin, the
//!    `examples/camera-python-display/effects` graphics-kernel
//!    package, and the subprocess bridges in `libs/streamlib-*-native`)
//!    has its non-test fn / method bodies scanned for `vulkan_inner`,
//!    `host_inner`, and `host_callbacks`. Hitting any of those from
//!    cdylib-resident code lands on `Texture::host_inner()`'s
//!    panic-guard at runtime: `vulkan_inner` on a `Texture` derefs
//!    `host_inner`, and `host_inner` / `host_callbacks` are the
//!    panic sites themselves. Tests (`#[cfg(test)]` items and the
//!    `mod tests` they live under) run host-side via `cargo test --lib`
//!    and don't hit the panic, so the visitor skips them.
//!
//! Both scans use `syn` AST walks; comments and string literals are
//! skipped automatically.
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

/// Identifiers banned inside engine-side constructor-class function
/// bodies.
const BANNED_IDENTS: &[&str] = &["host_inner", "host_callbacks"];

/// Identifiers banned inside cdylib-resident dispatch paths
/// (non-test fn / method bodies in any crate with `crate-type =
/// [..., "cdylib", ...]`).
///
/// - `vulkan_inner`: `Texture::vulkan_inner()` derefs `host_inner()`,
///   which panics under the cdylib-side `host_callbacks().is_some()`
///   guard. `PixelBufferRef::vulkan_inner` reads its `pub inner`
///   field directly and is technically safe, but no cdylib code in
///   the tree calls it today; banning the name everywhere avoids
///   the false-negative class where a future migration silently
///   reintroduces the `Texture` pattern.
/// - `host_inner`: the panic site directly. `pub(crate)` so cdylib
///   code can't reach it through typed Rust anyway, but matching
///   the bare identifier is belt-and-suspenders for path
///   expressions and qualified calls.
/// - `host_callbacks`: the cdylib-mode sentinel. Cdylib code that
///   needs to know "am I in cdylib mode?" should not use this —
///   the architecture is type-system enforcement, not runtime
///   branching.
const BANNED_DISPATCH_IDENTS: &[&str] = &["vulkan_inner", "host_inner", "host_callbacks"];

#[derive(Debug, PartialEq, Eq)]
pub struct Violation {
    pub file: PathBuf,
    pub impl_type: String,
    pub method: String,
    pub offending_ident: String,
    pub line: usize,
}

/// Cdylib-side dispatch-path violation: a fn or method body in a
/// crate that ships as a cdylib reached for one of the
/// host-only / panic-guarded identifiers.
#[derive(Debug, PartialEq, Eq)]
pub struct DispatchViolation {
    pub file: PathBuf,
    pub crate_name: String,
    /// Enclosing fn / method name. `None` for free-standing call
    /// sites at module scope (rare; syn still records them).
    pub method: Option<String>,
    pub offending_ident: String,
    pub line: usize,
}

pub fn run(workspace_root: &Path) -> Result<()> {
    let engine_violations = scan_workspace(workspace_root)?;
    let dispatch_violations = scan_cdylib_dispatch_paths(workspace_root)?;
    let total = engine_violations.len() + dispatch_violations.len();

    if total == 0 {
        println!(
            "✓ check-cdylib-reach: every Host* constructor in the engine RHI \
             stays clear of host_inner() / host_callbacks(), and every \
             workspace cdylib's dispatch paths stay clear of vulkan_inner() / \
             host_inner() / host_callbacks() — cdylib panic-guard path intact."
        );
        return Ok(());
    }

    if !engine_violations.is_empty() {
        eprintln!(
            "✗ check-cdylib-reach: {} engine-side violation(s) — Host* \
             constructor reached for a host-private guard, breaking the \
             cdylib direct-call path:",
            engine_violations.len()
        );
        for v in &engine_violations {
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
            "\nFix (engine-side):\n  \
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
    }

    if !dispatch_violations.is_empty() {
        if !engine_violations.is_empty() {
            eprintln!();
        }
        eprintln!(
            "✗ check-cdylib-reach: {} cdylib-side violation(s) — non-test \
             dispatch path in a cdylib crate reached for a host-only / \
             panic-guarded identifier:",
            dispatch_violations.len()
        );
        for v in &dispatch_violations {
            let method = v.method.as_deref().unwrap_or("<module scope>");
            eprintln!(
                "  {}:{}: crate {} :: fn {} references `{}`",
                v.file.display(),
                v.line,
                v.crate_name,
                method,
                v.offending_ident,
            );
        }
        eprintln!(
            "\nFix (cdylib-side):\n  \
             A non-test fn / method in a cdylib crate called `vulkan_inner()`, \
             `host_inner()`, or `host_callbacks()`. `Texture::vulkan_inner()` \
             derefs `Texture::host_inner()`, which panics under the cdylib-side \
             `host_callbacks().is_some()` guard (see\n  \
             `docs/architecture/cdylib-reachability.md`).\n  \
             Options:\n    \
               1. For raw `VkImage` / `VkImageView` access from a `Texture`, \
                  use `HostTextureExt::host_vulkan_texture_arc()` (the v10 \
                  FullAccess vtable slot — already in use by \
                  `packages/camera/src/camera_to_cuda_copy.rs`, \
                  `packages/h264/src/linux/encoder.rs`, \
                  `packages/h265/src/linux/encoder.rs`).\n    \
               2. If the call genuinely belongs in a host-only path (a \
                  `#[test]` or `#[cfg(test)]` helper), move it under \
                  `#[cfg(test)]` / `mod tests` so the lint skips it."
        );
    }

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

/// Scan every workspace crate whose Cargo.toml declares
/// `crate-type` containing `"cdylib"`. For each such crate, walk
/// every `.rs` file under `src/` and flag dispatch-path uses of
/// [`BANNED_DISPATCH_IDENTS`]. `#[cfg(test)]` items and modules are
/// skipped (those run host-side via `cargo test --lib`, where the
/// `host_callbacks()` guard returns `None` and the panic never
/// fires).
pub fn scan_cdylib_dispatch_paths(workspace_root: &Path) -> Result<Vec<DispatchViolation>> {
    let mut all = Vec::new();
    for (crate_dir, crate_name) in discover_cdylib_crates(workspace_root)? {
        let src_dir = crate_dir.join("src");
        if !src_dir.exists() {
            continue;
        }
        for entry in WalkDir::new(&src_dir).into_iter().filter_map(|e| e.ok()) {
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
            // Files that fail to parse (proc-macro-heavy generated
            // code, partial includes) are skipped — the lint can't
            // reason about them and false-negatives there are
            // strictly safer than false-positives on shape-mismatches.
            let Ok(file) = syn::parse_file(&src) else {
                continue;
            };
            let mut visitor = DispatchFileVisitor {
                file_path: relpath,
                crate_name: crate_name.clone(),
                current_method: None,
                violations: &mut all,
            };
            visitor.visit_file(&file);
        }
    }
    Ok(all)
}

/// True iff any attribute on `attrs` is `#[cfg(test)]` (or
/// `#[cfg(all(..., test, ...))]` / `#[cfg(any(test, ...))]` containing
/// a bare `test` identifier anywhere in the token stream). An
/// `#[cfg(any(test, ...))]` is treated the same way — the surrounding
/// item compiles only when `test` is set in at least one branch, so
/// it's a test-only item by the rule we care about.
fn has_cfg_test(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|attr| {
        if !attr.path().is_ident("cfg") {
            return false;
        }
        let Ok(list) = attr.meta.require_list() else {
            return false;
        };
        token_stream_contains_test_ident(list.tokens.clone())
    })
}

/// Recursively walk a token stream looking for the bare identifier
/// `test`. Descends into `Group`s so `cfg(all(test, ...))` and
/// `cfg(any(test, ...))` both match. Identifiers nested inside name=value
/// pairs (e.g. `target_family = "test"`) are intentionally not matched —
/// only `test` as a standalone ident counts.
fn token_stream_contains_test_ident(tokens: proc_macro2::TokenStream) -> bool {
    tokens.into_iter().any(|tt| match tt {
        proc_macro2::TokenTree::Ident(i) => i == "test",
        proc_macro2::TokenTree::Group(g) => {
            token_stream_contains_test_ident(g.stream())
        }
        _ => false,
    })
}

/// Read the `[package].name` field from a `Cargo.toml`. Returns
/// `None` for non-package manifests (workspace-only roots, etc.).
fn cargo_toml_package_name(toml_str: &str) -> Option<String> {
    let v: toml::Value = toml::from_str(toml_str).ok()?;
    v.get("package")?.get("name")?.as_str().map(String::from)
}

/// True iff the manifest's `[lib].crate-type` array contains
/// `"cdylib"`. Returns `false` for manifests with no `[lib]` table
/// or no `crate-type` array.
fn cargo_toml_has_cdylib(toml_str: &str) -> bool {
    let Ok(v) = toml::from_str::<toml::Value>(toml_str) else {
        return false;
    };
    let Some(arr) = v.get("lib").and_then(|l| l.get("crate-type")).and_then(|c| c.as_array())
    else {
        return false;
    };
    arr.iter().any(|e| e.as_str() == Some("cdylib"))
}

/// Walk the workspace and collect `(crate_dir, crate_name)` for
/// every crate whose Cargo.toml declares a cdylib crate-type. Skips
/// `target/`, hidden directories (`.claude/`, `.git/`), and the
/// workspace-root Cargo.toml itself.
fn discover_cdylib_crates(workspace_root: &Path) -> Result<Vec<(PathBuf, String)>> {
    let mut out = Vec::new();
    let walker = WalkDir::new(workspace_root)
        .into_iter()
        .filter_entry(|e| {
            // Prune target/, hidden dirs (`.git`, `.claude`), and
            // anything not a directory at this level — `.rs` files
            // and Cargo.toml files are kept.
            let name = e.file_name().to_string_lossy();
            if e.file_type().is_dir() {
                name != "target" && !name.starts_with('.')
            } else {
                true
            }
        });
    for entry in walker.filter_map(|e| e.ok()) {
        if entry.file_name() != "Cargo.toml" {
            continue;
        }
        let manifest = entry.path();
        let crate_dir = manifest.parent().unwrap_or(workspace_root);
        // Skip the workspace-root manifest (no [lib]; no crate-type).
        if crate_dir == workspace_root {
            continue;
        }
        let toml_str = fs::read_to_string(manifest)
            .with_context(|| format!("reading {}", manifest.display()))?;
        if !cargo_toml_has_cdylib(&toml_str) {
            continue;
        }
        let crate_name = cargo_toml_package_name(&toml_str).unwrap_or_else(|| {
            crate_dir
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "<unknown>".into())
        });
        out.push((crate_dir.to_path_buf(), crate_name));
    }
    Ok(out)
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

/// Visitor for the cdylib-side scan. Walks every fn / method body
/// in a cdylib crate's source files (excluding `#[cfg(test)]`
/// items) and pushes a [`DispatchViolation`] for each banned
/// identifier found in non-test bodies.
struct DispatchFileVisitor<'a> {
    file_path: PathBuf,
    crate_name: String,
    current_method: Option<String>,
    violations: &'a mut Vec<DispatchViolation>,
}

impl<'ast, 'a> Visit<'ast> for DispatchFileVisitor<'a> {
    fn visit_item_mod(&mut self, m: &'ast syn::ItemMod) {
        if has_cfg_test(&m.attrs) {
            return;
        }
        syn::visit::visit_item_mod(self, m);
    }

    fn visit_item_impl(&mut self, i: &'ast syn::ItemImpl) {
        if has_cfg_test(&i.attrs) {
            return;
        }
        syn::visit::visit_item_impl(self, i);
    }

    fn visit_impl_item_fn(&mut self, f: &'ast syn::ImplItemFn) {
        if has_cfg_test(&f.attrs) {
            return;
        }
        let prev = self.current_method.replace(f.sig.ident.to_string());
        let mut body = DispatchBodyVisitor::default();
        body.visit_block(&f.block);
        for hit in body.hits {
            self.violations.push(DispatchViolation {
                file: self.file_path.clone(),
                crate_name: self.crate_name.clone(),
                method: Some(f.sig.ident.to_string()),
                offending_ident: hit.ident,
                line: hit.line,
            });
        }
        self.current_method = prev;
    }

    fn visit_item_fn(&mut self, f: &'ast syn::ItemFn) {
        if has_cfg_test(&f.attrs) {
            return;
        }
        let prev = self.current_method.replace(f.sig.ident.to_string());
        let mut body = DispatchBodyVisitor::default();
        body.visit_block(&f.block);
        for hit in body.hits {
            self.violations.push(DispatchViolation {
                file: self.file_path.clone(),
                crate_name: self.crate_name.clone(),
                method: Some(f.sig.ident.to_string()),
                offending_ident: hit.ident,
                line: hit.line,
            });
        }
        self.current_method = prev;
    }
}

#[derive(Default)]
struct DispatchBodyVisitor {
    hits: Vec<Hit>,
}

impl<'ast> Visit<'ast> for DispatchBodyVisitor {
    fn visit_expr_method_call(&mut self, call: &'ast syn::ExprMethodCall) {
        let m = call.method.to_string();
        if BANNED_DISPATCH_IDENTS.contains(&m.as_str()) {
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
            if BANNED_DISPATCH_IDENTS.contains(&n.as_str()) {
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

    // ---- Cdylib-side dispatch-path scan tests ------------------------------
    //
    // These exercise `DispatchFileVisitor` directly against in-memory
    // source, bypassing the Cargo.toml discovery step. The discovery
    // helpers (`cargo_toml_has_cdylib`, `cargo_toml_package_name`) have
    // their own focused tests below.

    fn scan_dispatch(src: &str) -> Vec<DispatchViolation> {
        let file = syn::parse_file(src).expect("parse");
        let mut violations = Vec::new();
        let mut visitor = DispatchFileVisitor {
            file_path: PathBuf::from("test.rs"),
            crate_name: "test-cdylib".into(),
            current_method: None,
            violations: &mut violations,
        };
        visitor.visit_file(&file);
        violations
    }

    #[test]
    fn dispatch_flags_vulkan_inner_in_method() {
        // The actual #1065 bug shape — a kernel wrapper's dispatch
        // method reaches `tex.vulkan_inner().image()` to grab the
        // raw `VkImage`. Panics under the cdylib `host_callbacks()`
        // guard at runtime.
        let src = r#"
            impl SandboxedBlendingCompositor {
                pub fn dispatch(&self, tex: &Texture) -> Result<()> {
                    let image = tex.vulkan_inner().image().ok_or_else(|| Error::E)?;
                    Ok(())
                }
            }
        "#;
        let v = scan_dispatch(src);
        assert_eq!(v.len(), 1, "expected one violation, got {v:?}");
        assert_eq!(v[0].offending_ident, "vulkan_inner");
        assert_eq!(v[0].method.as_deref(), Some("dispatch"));
    }

    #[test]
    fn dispatch_flags_multiple_sites() {
        // Mirrors the actual count in
        // `blending_compositor_kernel.rs::dispatch` — three reaches
        // (input image, output image, output image_view).
        let src = r#"
            impl Kernel {
                pub fn dispatch(&self, t: &Texture, out: &Texture) -> Result<()> {
                    let _a = t.vulkan_inner().image();
                    let _b = out.vulkan_inner().image();
                    let _c = out.vulkan_inner().image_view();
                    Ok(())
                }
            }
        "#;
        let v = scan_dispatch(src);
        assert_eq!(v.len(), 3, "expected three violations, got {v:?}");
        assert!(v.iter().all(|x| x.offending_ident == "vulkan_inner"));
    }

    #[test]
    fn dispatch_skips_cfg_test_modules() {
        // Test-only fill / readback helpers in a `mod tests` block
        // run host-side under `cargo test --lib`, where
        // `host_callbacks()` is `None` and the panic never fires.
        // The lint must not flag them.
        let src = r#"
            impl Kernel {
                pub fn dispatch(&self, t: &Texture) -> Result<()> {
                    // Real dispatch — would normally be a violation,
                    // but this test only checks that the cfg(test)
                    // module below is skipped.
                    Ok(())
                }
            }

            #[cfg(test)]
            mod tests {
                use super::*;

                fn fill_solid(texture: &Texture) {
                    let _image = texture.vulkan_inner().image().expect("image");
                }

                #[test]
                fn visual_smoke() {
                    let _image = input.vulkan_inner().image().expect("image");
                }
            }
        "#;
        let v = scan_dispatch(src);
        assert!(
            v.is_empty(),
            "cfg(test) module bodies must not be flagged, got {v:?}"
        );
    }

    #[test]
    fn dispatch_skips_cfg_test_methods() {
        // Per-method `#[cfg(test)]` annotation must skip the body
        // even when the enclosing module isn't gated.
        let src = r#"
            impl Kernel {
                pub fn dispatch(&self, t: &Texture) -> Result<()> {
                    Ok(())
                }

                #[cfg(test)]
                fn test_helper(&self, t: &Texture) {
                    let _image = t.vulkan_inner().image().expect("image");
                }
            }
        "#;
        let v = scan_dispatch(src);
        assert!(v.is_empty(), "cfg(test) method bodies must not be flagged");
    }

    #[test]
    fn dispatch_flags_host_inner_path_expr() {
        // Path expression matching `crate::path::host_inner` —
        // belt-and-suspenders for fully-qualified call shapes.
        let src = r#"
            fn outer(x: &T) -> u32 {
                if crate::plugin::host_callbacks().is_some() {
                    return 0;
                }
                x.host_inner();
                0
            }
        "#;
        let v = scan_dispatch(src);
        assert_eq!(v.len(), 2);
        let idents: Vec<&str> = v.iter().map(|x| x.offending_ident.as_str()).collect();
        assert!(idents.contains(&"host_callbacks"));
        assert!(idents.contains(&"host_inner"));
    }

    #[test]
    fn dispatch_ignores_docstring_and_string_literal() {
        let src = r#"
            impl Kernel {
                /// This dispatch does NOT call vulkan_inner() or
                /// host_inner() — see the rule in CLAUDE.md.
                pub fn dispatch(&self) -> Result<(), String> {
                    Err("vulkan_inner failed (just a message)".into())
                }
            }
        "#;
        let v = scan_dispatch(src);
        assert!(v.is_empty(), "comments and strings must not be flagged");
    }

    #[test]
    fn dispatch_flags_free_fn_at_module_scope() {
        let src = r#"
            fn helper(t: &Texture) -> u32 {
                let _i = t.vulkan_inner().image();
                0
            }
        "#;
        let v = scan_dispatch(src);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].method.as_deref(), Some("helper"));
    }

    // ---- Cargo.toml discovery helpers ---------------------------------------

    #[test]
    fn cargo_toml_has_cdylib_detects_workspace_plugin() {
        let toml = r#"
            [package]
            name = "streamlib-camera"
            version = "0.0.0"
            edition = "2024"

            [lib]
            crate-type = ["rlib", "cdylib"]
        "#;
        assert!(cargo_toml_has_cdylib(toml));
        assert_eq!(
            cargo_toml_package_name(toml).as_deref(),
            Some("streamlib-camera")
        );
    }

    #[test]
    fn cargo_toml_has_cdylib_skips_pure_rlib() {
        let toml = r#"
            [package]
            name = "streamlib-jtd-codegen"
            version = "0.0.0"
            edition = "2024"
        "#;
        // No [lib] section at all — falls back to default rlib.
        assert!(!cargo_toml_has_cdylib(toml));
    }

    #[test]
    fn cargo_toml_has_cdylib_skips_rlib_only_lib_section() {
        let toml = r#"
            [package]
            name = "streamlib-misc"
            version = "0.0.0"
            edition = "2024"

            [lib]
            crate-type = ["rlib"]
        "#;
        assert!(!cargo_toml_has_cdylib(toml));
    }

    #[test]
    fn cargo_toml_has_cdylib_handles_workspace_root() {
        // Workspace-root manifests have no [package] / [lib].
        let toml = r#"
            [workspace]
            members = ["packages/*"]
        "#;
        assert!(!cargo_toml_has_cdylib(toml));
        assert!(cargo_toml_package_name(toml).is_none());
    }

    #[test]
    fn has_cfg_test_detects_bare_form() {
        let attrs: syn::ItemFn = syn::parse_str(
            "#[cfg(test)] fn x() {}",
        )
        .expect("parse");
        assert!(has_cfg_test(&attrs.attrs));
    }

    #[test]
    fn has_cfg_test_detects_all_combinator() {
        let attrs: syn::ItemFn = syn::parse_str(
            "#[cfg(all(test, target_os = \"linux\"))] fn x() {}",
        )
        .expect("parse");
        assert!(has_cfg_test(&attrs.attrs));
    }

    #[test]
    fn has_cfg_test_ignores_non_test_cfg() {
        let attrs: syn::ItemFn = syn::parse_str(
            "#[cfg(target_os = \"linux\")] fn x() {}",
        )
        .expect("parse");
        assert!(!has_cfg_test(&attrs.attrs));
    }

    #[test]
    fn has_cfg_test_ignores_non_cfg_attrs() {
        let attrs: syn::ItemFn = syn::parse_str(
            "#[allow(dead_code)] fn x() {}",
        )
        .expect("parse");
        assert!(!has_cfg_test(&attrs.attrs));
    }
}
