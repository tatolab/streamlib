// Copyright (c) 2026 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! CI gate enforcing the all-dynamic registration rule for processor
//! factories.
//!
//! The engine substrate is an empty registry; processors land in
//! `PROCESSOR_REGISTRY` through one of two paths:
//!
//! 1. Cdylib packages loaded via `runtime.load_project(...)` /
//!    `runtime.load_package(...)`, which trip `STREAMLIB_PLUGIN` and
//!    call the host's `processor_register` callback.
//! 2. In-process Rust callers invoking
//!    `PROCESSOR_REGISTRY.register::<P>()` directly.
//!
//! The link-time `inventory::submit!(FactoryRegistration { ... })`
//! emission the `#[processor]` macro used to do is gone. Anyone
//! reintroducing it would bypass the dynamic-load model — `Runner::new()`
//! would silently grow non-empty in builds that link the offending crate,
//! processors would register before any `load_project` call, and
//! cross-rustc-version plugin builds would start to "work" via the
//! force-link path that the milestone wants gone.
//!
//! This gate scans `.rs` files under `packages/`, `libs/`, and `examples/`
//! and fails when any non-`#[cfg(test)]` item contains
//! `inventory::submit!(FactoryRegistration ...)` (in macro-call or
//! macro-token shape). The `RuntimeInitHookRegistration` inventory used
//! by `core::runtime_hooks` is a separate registration system and is NOT
//! flagged — only `FactoryRegistration` submissions are.
//!
//! Comments, doc strings, and `#[cfg(test)]`-gated code are exempt: the
//! Rust AST walk skips them automatically by construction.

use anyhow::{Context, Result};
use std::fs;
use std::path::{Path, PathBuf};
use syn::visit::Visit;
use walkdir::WalkDir;

const SCAN_PARENTS: &[&str] = &["libs", "packages", "examples"];

const SKIP_PATH_FRAGMENTS: &[&str] = &[
    "/target/",
    "/_generated_/",
    "/node_modules/",
    "/.git/",
];

/// Marker comment that exempts an entire file from this gate. Reserved
/// for the gate's own definition (which must contain the banned token
/// literally) and any future legitimate-but-unusual case reviewers
/// approve explicitly.
const ALLOW_FILE_PRAGMA: &str = "check-no-inventory-submit:allow-file";

#[derive(Debug, PartialEq, Eq)]
pub struct Violation {
    pub file: PathBuf,
    pub line: usize,
}

pub fn run(workspace_root: &Path) -> Result<()> {
    let violations = scan_workspace(workspace_root)?;
    if violations.is_empty() {
        println!(
            "✓ check-no-inventory-submit: no `inventory::submit!(FactoryRegistration ...)` \
             in live code — the engine substrate stays empty by construction."
        );
        return Ok(());
    }
    eprintln!(
        "✗ check-no-inventory-submit: {} violation(s) — link-time \
         processor registration reintroduced:",
        violations.len()
    );
    for v in &violations {
        eprintln!("  {}:{}: inventory::submit!(FactoryRegistration ...)", v.file.display(), v.line);
    }
    eprintln!(
        "\nFix:\n  \
         The engine substrate is empty by design. Register processors \
         through one of:\n    \
           1. Cdylib `export_plugin!(...)` — the runtime dlopens the \
              cdylib and registers via the plugin ABI's STREAMLIB_PLUGIN \
              callback.\n    \
           2. In-process `PROCESSOR_REGISTRY.register::<P>()` — for \
              engine-internal tests / mocks / inline registrations.\n  \
         The `#[processor]` macro no longer emits any registration. See \
         issue #793 + the All-Dynamic Package Loading milestone."
    );
    anyhow::bail!("check-no-inventory-submit failed");
}

pub fn scan_workspace(workspace_root: &Path) -> Result<Vec<Violation>> {
    let mut violations = Vec::new();
    for parent in SCAN_PARENTS {
        let dir = workspace_root.join(parent);
        if !dir.exists() {
            continue;
        }
        scan_dir(&dir, &mut violations)?;
    }
    Ok(violations)
}

fn scan_dir(dir: &Path, violations: &mut Vec<Violation>) -> Result<()> {
    for entry in WalkDir::new(dir).into_iter().filter_map(|e| e.ok()) {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let path_str = path.to_string_lossy();
        if SKIP_PATH_FRAGMENTS
            .iter()
            .any(|frag| path_str.contains(frag))
        {
            continue;
        }
        if path.extension().and_then(|e| e.to_str()) != Some("rs") {
            continue;
        }
        scan_file(path, violations)?;
    }
    Ok(())
}

fn scan_file(path: &Path, violations: &mut Vec<Violation>) -> Result<()> {
    let body = fs::read_to_string(path)
        .with_context(|| format!("Failed to read {}", path.display()))?;
    if body.contains(ALLOW_FILE_PRAGMA) {
        return Ok(());
    }
    let file = match syn::parse_file(&body) {
        Ok(f) => f,
        Err(_) => return Ok(()), // unparseable — leave to rustc to surface
    };
    if file.attrs.iter().any(is_cfg_test_attr) {
        return Ok(());
    }
    let mut visitor = RustVisitor {
        path: path.to_path_buf(),
        violations,
    };
    visitor.visit_file(&file);
    Ok(())
}

struct RustVisitor<'a> {
    path: PathBuf,
    violations: &'a mut Vec<Violation>,
}

impl<'ast, 'a> Visit<'ast> for RustVisitor<'a> {
    fn visit_attribute(&mut self, _attr: &'ast syn::Attribute) {
        // Don't descend into attributes — skips `#[doc = "..."]` strings
        // and other attribute literals that might mention the banned
        // token in prose.
    }

    fn visit_item(&mut self, item: &'ast syn::Item) {
        if let Some(attrs) = item_attrs(item) {
            if attrs.iter().any(is_cfg_test_attr) {
                return;
            }
        }
        syn::visit::visit_item(self, item);
    }

    fn visit_impl_item(&mut self, item: &'ast syn::ImplItem) {
        let attrs: &[syn::Attribute] = match item {
            syn::ImplItem::Const(x) => &x.attrs,
            syn::ImplItem::Fn(x) => &x.attrs,
            syn::ImplItem::Type(x) => &x.attrs,
            syn::ImplItem::Macro(x) => &x.attrs,
            _ => &[],
        };
        if attrs.iter().any(is_cfg_test_attr) {
            return;
        }
        syn::visit::visit_impl_item(self, item);
    }

    fn visit_item_macro(&mut self, mac: &'ast syn::ItemMacro) {
        if mac.attrs.iter().any(is_cfg_test_attr) {
            return;
        }
        // Fall through to `visit_macro` via the default traversal so
        // we only count each macro invocation once.
        syn::visit::visit_item_macro(self, mac);
    }

    fn visit_macro(&mut self, mac: &'ast syn::Macro) {
        check_macro(mac, &self.path, self.violations);
        syn::visit::visit_macro(self, mac);
    }
}

/// True iff `mac` is `inventory::submit!{...}` AND the token stream
/// inside the braces references `FactoryRegistration`. Matches the
/// macro's `path::path::submit` shape regardless of the leading
/// `::streamlib::sdk::` qualifier — we only care that the final two
/// segments are `inventory::submit`.
fn check_macro(mac: &syn::Macro, path: &Path, violations: &mut Vec<Violation>) {
    let segs = &mac.path.segments;
    let n = segs.len();
    if n < 2 {
        return;
    }
    if segs[n - 2].ident != "inventory" || segs[n - 1].ident != "submit" {
        return;
    }
    if !tokens_reference_factory_registration(mac.tokens.clone()) {
        return;
    }
    violations.push(Violation {
        file: path.to_path_buf(),
        line: segs[n - 1].ident.span().start().line,
    });
}

fn tokens_reference_factory_registration(stream: proc_macro2::TokenStream) -> bool {
    for tt in stream {
        match tt {
            proc_macro2::TokenTree::Ident(i) if i == "FactoryRegistration" => return true,
            proc_macro2::TokenTree::Group(g) => {
                if tokens_reference_factory_registration(g.stream()) {
                    return true;
                }
            }
            _ => {}
        }
    }
    false
}

/// True if this `#[cfg(...)]` attribute contains a bare `test` predicate
/// at any nesting level (`#[cfg(test)]`, `#[cfg(all(test, ...))]`,
/// `#[cfg(any(test, ...))]`, etc.).
fn is_cfg_test_attr(attr: &syn::Attribute) -> bool {
    if !attr.path().is_ident("cfg") {
        return false;
    }
    let Ok(list) = attr.meta.require_list() else {
        return false;
    };
    tokens_contain_bare_test(list.tokens.clone())
}

fn tokens_contain_bare_test(stream: proc_macro2::TokenStream) -> bool {
    for tt in stream {
        match tt {
            proc_macro2::TokenTree::Ident(i) if i == "test" => return true,
            proc_macro2::TokenTree::Group(g) => {
                if tokens_contain_bare_test(g.stream()) {
                    return true;
                }
            }
            _ => {}
        }
    }
    false
}

fn item_attrs(item: &syn::Item) -> Option<&[syn::Attribute]> {
    Some(match item {
        syn::Item::Const(x) => &x.attrs,
        syn::Item::Enum(x) => &x.attrs,
        syn::Item::ExternCrate(x) => &x.attrs,
        syn::Item::Fn(x) => &x.attrs,
        syn::Item::ForeignMod(x) => &x.attrs,
        syn::Item::Impl(x) => &x.attrs,
        syn::Item::Macro(x) => &x.attrs,
        syn::Item::Mod(x) => &x.attrs,
        syn::Item::Static(x) => &x.attrs,
        syn::Item::Struct(x) => &x.attrs,
        syn::Item::Trait(x) => &x.attrs,
        syn::Item::TraitAlias(x) => &x.attrs,
        syn::Item::Type(x) => &x.attrs,
        syn::Item::Union(x) => &x.attrs,
        syn::Item::Use(x) => &x.attrs,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn write(dir: &Path, rel: &str, body: &str) -> PathBuf {
        let path = dir.join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&path, body).unwrap();
        path
    }

    #[test]
    fn passes_on_clean_workspace() {
        let tmp = TempDir::new().unwrap();
        write(
            tmp.path(),
            "libs/foo/src/lib.rs",
            r#"
pub fn hello() -> &'static str { "world" }

inventory::submit!(RuntimeInitHookRegistration::new::<MyHook>());
"#,
        );
        let violations = scan_workspace(tmp.path()).unwrap();
        assert!(violations.is_empty(), "got {violations:?}");
    }

    #[test]
    fn flags_factory_registration_submit() {
        let tmp = TempDir::new().unwrap();
        write(
            tmp.path(),
            "libs/foo/src/lib.rs",
            r#"
inventory::submit! {
    FactoryRegistration {
        register_fn: |factory| factory.register::<Processor>(),
    }
}
"#,
        );
        let violations = scan_workspace(tmp.path()).unwrap();
        assert_eq!(violations.len(), 1, "got {violations:?}");
    }

    #[test]
    fn flags_fully_qualified_submit() {
        let tmp = TempDir::new().unwrap();
        write(
            tmp.path(),
            "libs/foo/src/lib.rs",
            r#"
::streamlib::sdk::inventory::submit! {
    ::streamlib::sdk::processors::macro_codegen::FactoryRegistration {
        register_fn: |factory| factory.register::<Processor>(),
    }
}
"#,
        );
        let violations = scan_workspace(tmp.path()).unwrap();
        assert_eq!(violations.len(), 1, "got {violations:?}");
    }

    #[test]
    fn skips_runtime_init_hook_registration() {
        let tmp = TempDir::new().unwrap();
        write(
            tmp.path(),
            "libs/foo/src/lib.rs",
            r#"
inventory::submit! {
    RuntimeInitHookRegistration::new::<MyHook>()
}
"#,
        );
        let violations = scan_workspace(tmp.path()).unwrap();
        assert!(violations.is_empty(), "got {violations:?}");
    }

    #[test]
    fn skips_cfg_test_item() {
        let tmp = TempDir::new().unwrap();
        write(
            tmp.path(),
            "libs/foo/src/lib.rs",
            r#"
pub fn ok() {}

#[cfg(test)]
mod tests {
    inventory::submit! {
        FactoryRegistration {
            register_fn: |factory| factory.register::<Processor>(),
        }
    }
}
"#,
        );
        let violations = scan_workspace(tmp.path()).unwrap();
        assert!(violations.is_empty(), "got {violations:?}");
    }

    #[test]
    fn skips_doc_comment_mention() {
        let tmp = TempDir::new().unwrap();
        write(
            tmp.path(),
            "libs/foo/src/lib.rs",
            r#"
/// Historical: the macro used to emit
/// `inventory::submit!(FactoryRegistration { ... })`.
pub fn ok() {}
"#,
        );
        let violations = scan_workspace(tmp.path()).unwrap();
        assert!(violations.is_empty(), "got {violations:?}");
    }

    #[test]
    fn skips_allow_file_pragma() {
        let tmp = TempDir::new().unwrap();
        write(
            tmp.path(),
            "libs/foo/src/lib.rs",
            r#"
// check-no-inventory-submit:allow-file
inventory::submit! {
    FactoryRegistration {
        register_fn: |factory| factory.register::<Processor>(),
    }
}
"#,
        );
        let violations = scan_workspace(tmp.path()).unwrap();
        assert!(violations.is_empty(), "got {violations:?}");
    }

    #[test]
    fn skips_target_and_generated_dirs() {
        let tmp = TempDir::new().unwrap();
        write(
            tmp.path(),
            "libs/foo/target/build/lib.rs",
            r#"
inventory::submit! {
    FactoryRegistration { register_fn: |f| f.register::<P>() }
}
"#,
        );
        write(
            tmp.path(),
            "libs/foo/_generated_/whatever.rs",
            r#"
inventory::submit! {
    FactoryRegistration { register_fn: |f| f.register::<P>() }
}
"#,
        );
        let violations = scan_workspace(tmp.path()).unwrap();
        assert!(violations.is_empty(), "got {violations:?}");
    }

    #[test]
    fn flags_violation_in_example() {
        let tmp = TempDir::new().unwrap();
        write(
            tmp.path(),
            "examples/demo/src/main.rs",
            r#"
inventory::submit! {
    FactoryRegistration { register_fn: |f| f.register::<P>() }
}

fn main() {}
"#,
        );
        let violations = scan_workspace(tmp.path()).unwrap();
        assert_eq!(violations.len(), 1, "got {violations:?}");
    }
}
