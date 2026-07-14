// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! CI lint enforcing milestone-10's structured-identifier rule.
//!
//! Reverse-DNS schema literals (`com.tatolab.*`, `com.streamlib.*`) are
//! the legacy joined-string identifier shape. Milestone-10 ships the
//! `@org/package/Type@version` grammar with `SchemaIdent { org, package,
//! type, version }` records on every wire surface. This lint catches
//! anyone re-introducing the legacy shape in live code.
//!
//! Contexts that are NOT flagged:
//!
//! - Apple platform code (`*/apple/*` path segments) — DispatchQueue
//!   labels and XPC mach-service names legitimately use reverse-DNS.
//! - Test code — items under `#[cfg(test)]`, files under `tests/`,
//!   files ending in `_test.rs` / `_tests.rs`. Legacy-grammar
//!   conversion fixtures live in tests to lock the rejection behavior.
//! - Rust doc comments / line comments — historical context survives in
//!   prose.
//!
//! See `docs/architecture/schema-identity-and-packaging.md`.

// check-no-reverse-dns:allow-file — this file defines the banned
// prefixes and so must contain them literally.

use anyhow::{Context, Result};
use std::fs;
use std::path::{Path, PathBuf};
use syn::visit::Visit;
use walkdir::WalkDir;

const SCAN_PARENTS: &[&str] = &["runtime", "sdk", "adapters", "tools", "vendor", "packages", "examples", "xtask"];

const SKIP_PATH_FRAGMENTS: &[&str] = &[
    "/target/",
    "/_generated_/",
    "/node_modules/",
    "/.git/",
    "/tests/",
    "/apple/",
];

const SKIP_FILE_SUFFIXES: &[&str] = &["_test.rs", "_tests.rs"];

const SCAN_EXTENSIONS: &[&str] = &["rs", "yaml", "yml", "toml", "json", "jsonc"];

const BANNED_PREFIXES: &[&str] = &["com.tatolab.", "com.streamlib."];

/// Marker comment that exempts an entire file from this lint. Used by
/// the lint definition itself (which has to contain the banned prefixes
/// literally) and by any future legitimate-but-unusual case that
/// reviewers approve explicitly.
const ALLOW_FILE_PRAGMA: &str = "check-no-reverse-dns:allow-file";

#[derive(Debug, PartialEq, Eq)]
pub struct LintViolation {
    pub file: PathBuf,
    pub line: usize,
    pub literal: String,
}

pub fn run(workspace_root: &Path) -> Result<()> {
    let violations = lint_workspace(workspace_root)?;
    if violations.is_empty() {
        println!(
            "✓ check-no-reverse-dns: no `com.tatolab.*` / `com.streamlib.*` literals in live code"
        );
        return Ok(());
    }
    eprintln!("✗ check-no-reverse-dns: {} violation(s)", violations.len());
    for v in &violations {
        eprintln!(
            "  {}:{}: legacy reverse-DNS literal `{}` — use the structured `@org/package/Type` identifier instead. See docs/architecture/schema-identity-and-packaging.md",
            v.file.display(),
            v.line,
            v.literal,
        );
    }
    anyhow::bail!("reverse-DNS lint failed: {} violation(s)", violations.len());
}

pub fn lint_workspace(workspace_root: &Path) -> Result<Vec<LintViolation>> {
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

fn scan_dir(dir: &Path, violations: &mut Vec<LintViolation>) -> Result<()> {
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
        let file_name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
        if SKIP_FILE_SUFFIXES
            .iter()
            .any(|sfx| file_name.ends_with(sfx))
        {
            continue;
        }
        let extension = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        if !SCAN_EXTENSIONS.contains(&extension) {
            continue;
        }
        scan_file(path, extension, violations)?;
    }
    Ok(())
}

fn scan_file(path: &Path, extension: &str, violations: &mut Vec<LintViolation>) -> Result<()> {
    let body =
        fs::read_to_string(path).with_context(|| format!("Failed to read {}", path.display()))?;
    if body.contains(ALLOW_FILE_PRAGMA) {
        return Ok(());
    }
    match extension {
        "rs" => scan_rust(path, &body, violations),
        _ => scan_line_based(path, &body, extension, violations),
    }
    Ok(())
}

/// Walk the Rust AST and flag every string literal in non-test items
/// that contains a banned prefix. AST walking ignores comments and lets
/// us reject `#[cfg(test)]`-gated items by attribute inspection.
fn scan_rust(path: &Path, body: &str, violations: &mut Vec<LintViolation>) {
    let file = match syn::parse_file(body) {
        Ok(f) => f,
        Err(_) => return, // unparseable — leave to rustc to surface
    };
    if file.attrs.iter().any(is_cfg_test_attr) {
        return;
    }
    let mut visitor = RustVisitor { path, violations };
    visitor.visit_file(&file);
}

struct RustVisitor<'a> {
    path: &'a Path,
    violations: &'a mut Vec<LintViolation>,
}

impl<'ast, 'a> Visit<'ast> for RustVisitor<'a> {
    fn visit_attribute(&mut self, _attr: &'ast syn::Attribute) {
        // Don't descend into attributes — skips `#[doc = "..."]` strings
        // and other attribute literals. Item-level cfg(test) skipping
        // happens via the per-item visit overrides below.
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

    fn visit_trait_item(&mut self, item: &'ast syn::TraitItem) {
        let attrs: &[syn::Attribute] = match item {
            syn::TraitItem::Const(x) => &x.attrs,
            syn::TraitItem::Fn(x) => &x.attrs,
            syn::TraitItem::Type(x) => &x.attrs,
            syn::TraitItem::Macro(x) => &x.attrs,
            _ => &[],
        };
        if attrs.iter().any(is_cfg_test_attr) {
            return;
        }
        syn::visit::visit_trait_item(self, item);
    }

    fn visit_expr(&mut self, expr: &'ast syn::Expr) {
        if let Some(attrs) = expr_attrs(expr) {
            if attrs.iter().any(is_cfg_test_attr) {
                return;
            }
        }
        syn::visit::visit_expr(self, expr);
    }

    fn visit_lit_str(&mut self, lit: &'ast syn::LitStr) {
        let value = lit.value();
        for prefix in BANNED_PREFIXES {
            if value.contains(prefix) {
                let line = lit.span().start().line;
                self.violations.push(LintViolation {
                    file: self.path.to_path_buf(),
                    line,
                    literal: value.clone(),
                });
                return;
            }
        }
    }
}

/// Non-Rust file scan: strip line comments, search each line for banned
/// prefixes, capture the surrounding identifier-shaped token.
fn scan_line_based(path: &Path, body: &str, extension: &str, violations: &mut Vec<LintViolation>) {
    let comment_prefix = match extension {
        "yaml" | "yml" | "toml" => "#",
        "json" | "jsonc" => "//",
        _ => "",
    };
    for (idx, line) in body.lines().enumerate() {
        let scanned = strip_line_comment(line, comment_prefix);
        for prefix in BANNED_PREFIXES {
            if let Some(pos) = scanned.find(prefix) {
                let tail = &scanned[pos..];
                let end = tail
                    .find(|c: char| {
                        !c.is_ascii_alphanumeric() && c != '.' && c != '_' && c != '@' && c != '-'
                    })
                    .unwrap_or(tail.len());
                let literal = tail[..end].to_string();
                violations.push(LintViolation {
                    file: path.to_path_buf(),
                    line: idx + 1,
                    literal,
                });
                break;
            }
        }
    }
}

fn strip_line_comment<'a>(line: &'a str, prefix: &str) -> &'a str {
    if prefix.is_empty() {
        return line;
    }
    match line.split_once(prefix) {
        Some((before, _)) => before,
        None => line,
    }
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

fn expr_attrs(expr: &syn::Expr) -> Option<&[syn::Attribute]> {
    Some(match expr {
        syn::Expr::Array(x) => &x.attrs,
        syn::Expr::Block(x) => &x.attrs,
        syn::Expr::Call(x) => &x.attrs,
        syn::Expr::Closure(x) => &x.attrs,
        syn::Expr::ForLoop(x) => &x.attrs,
        syn::Expr::If(x) => &x.attrs,
        syn::Expr::Let(x) => &x.attrs,
        syn::Expr::Loop(x) => &x.attrs,
        syn::Expr::Match(x) => &x.attrs,
        syn::Expr::MethodCall(x) => &x.attrs,
        syn::Expr::While(x) => &x.attrs,
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
            "runtime/foo/src/lib.rs",
            "pub fn hello() -> &'static str { \"@tatolab/core/VideoFrame\" }\n",
        );
        write(
            tmp.path(),
            "runtime/foo/streamlib.yaml",
            "schemas:\n  VideoFrame:\n    package: \"@tatolab/core\"\n",
        );
        let violations = lint_workspace(tmp.path()).unwrap();
        assert!(violations.is_empty(), "got {violations:?}");
    }

    #[test]
    fn flags_reverse_dns_in_rust_production_code() {
        let tmp = TempDir::new().unwrap();
        write(
            tmp.path(),
            "runtime/foo/src/lib.rs",
            "pub fn legacy() -> &'static str { \"com.tatolab.foo.bar\" }\n",
        );
        let violations = lint_workspace(tmp.path()).unwrap();
        assert_eq!(violations.len(), 1, "got {violations:?}");
        assert!(violations[0].literal.contains("com.tatolab.foo.bar"));
    }

    #[test]
    fn flags_reverse_dns_in_yaml() {
        let tmp = TempDir::new().unwrap();
        write(
            tmp.path(),
            "runtime/foo/streamlib.yaml",
            "schemas:\n  Foo:\n    file: com.streamlib.foo.config.yaml\n",
        );
        let violations = lint_workspace(tmp.path()).unwrap();
        assert_eq!(violations.len(), 1, "got {violations:?}");
        assert!(violations[0].literal.starts_with("com.streamlib."));
    }

    #[test]
    fn skips_rust_cfg_test_module() {
        let tmp = TempDir::new().unwrap();
        write(
            tmp.path(),
            "runtime/foo/src/lib.rs",
            "pub fn ok() {}\n\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn legacy_grammar_fixture() {\n        let _ = \"com.tatolab.foo.bar\";\n    }\n}\n",
        );
        let violations = lint_workspace(tmp.path()).unwrap();
        assert!(violations.is_empty(), "got {violations:?}");
    }

    #[test]
    fn skips_rust_cfg_test_function() {
        let tmp = TempDir::new().unwrap();
        write(
            tmp.path(),
            "runtime/foo/src/lib.rs",
            "pub fn ok() {}\n\n#[cfg(test)]\nfn legacy() -> &'static str { \"com.tatolab.foo\" }\n",
        );
        let violations = lint_workspace(tmp.path()).unwrap();
        assert!(violations.is_empty(), "got {violations:?}");
    }

    #[test]
    fn skips_rust_doc_comment() {
        let tmp = TempDir::new().unwrap();
        write(
            tmp.path(),
            "runtime/foo/src/lib.rs",
            "/// Legacy form was `com.tatolab.foo.bar`.\npub fn ok() {}\n",
        );
        let violations = lint_workspace(tmp.path()).unwrap();
        assert!(violations.is_empty(), "got {violations:?}");
    }

    #[test]
    fn skips_apple_path() {
        let tmp = TempDir::new().unwrap();
        write(
            tmp.path(),
            "packages/foo/src/apple/audio.rs",
            "pub fn label() -> &'static str { \"com.tatolab.streamlib.audio\" }\n",
        );
        let violations = lint_workspace(tmp.path()).unwrap();
        assert!(violations.is_empty(), "got {violations:?}");
    }

    #[test]
    fn skips_tests_dir() {
        let tmp = TempDir::new().unwrap();
        write(
            tmp.path(),
            "runtime/foo/tests/integration.rs",
            "fn main() { let _ = \"com.tatolab.foo\"; }\n",
        );
        let violations = lint_workspace(tmp.path()).unwrap();
        assert!(violations.is_empty(), "got {violations:?}");
    }

    #[test]
    fn skips_test_filename_suffixes() {
        let tmp = TempDir::new().unwrap();
        write(
            tmp.path(),
            "runtime/foo/src/foo_test.rs",
            "fn main() { let _ = \"com.tatolab.foo\"; }\n",
        );
        write(
            tmp.path(),
            "runtime/foo/src/integration_tests.rs",
            "fn main() { let _ = \"com.streamlib.foo\"; }\n",
        );
        let violations = lint_workspace(tmp.path()).unwrap();
        assert!(violations.is_empty(), "got {violations:?}");
    }

    #[test]
    fn skips_yaml_comment() {
        let tmp = TempDir::new().unwrap();
        write(
            tmp.path(),
            "runtime/foo/streamlib.yaml",
            "# Historical: the old form was com.streamlib.foo.config\nschemas: {}\n",
        );
        let violations = lint_workspace(tmp.path()).unwrap();
        assert!(violations.is_empty(), "got {violations:?}");
    }

    #[test]
    fn skips_target_and_generated_dirs() {
        let tmp = TempDir::new().unwrap();
        write(
            tmp.path(),
            "runtime/foo/target/build/lib.rs",
            "pub fn legacy() -> &'static str { \"com.tatolab.foo\" }\n",
        );
        write(
            tmp.path(),
            "runtime/foo/_generated_/whatever.rs",
            "pub fn legacy() -> &'static str { \"com.tatolab.foo\" }\n",
        );
        let violations = lint_workspace(tmp.path()).unwrap();
        assert!(violations.is_empty(), "got {violations:?}");
    }

    #[test]
    fn cfg_all_test_target_os_skips() {
        let tmp = TempDir::new().unwrap();
        write(
            tmp.path(),
            "runtime/foo/src/lib.rs",
            "pub fn ok() {}\n\n#[cfg(all(test, target_os = \"linux\"))]\nfn legacy() -> &'static str { \"com.tatolab.foo\" }\n",
        );
        let violations = lint_workspace(tmp.path()).unwrap();
        assert!(violations.is_empty(), "got {violations:?}");
    }

    #[test]
    fn skips_file_with_allow_pragma() {
        let tmp = TempDir::new().unwrap();
        write(
            tmp.path(),
            "runtime/foo/src/lib.rs",
            "// check-no-reverse-dns:allow-file\npub fn legacy() -> &'static str { \"com.tatolab.foo\" }\n",
        );
        let violations = lint_workspace(tmp.path()).unwrap();
        assert!(violations.is_empty(), "got {violations:?}");
    }

    #[test]
    fn cfg_target_os_alone_does_not_skip() {
        let tmp = TempDir::new().unwrap();
        write(
            tmp.path(),
            "runtime/foo/src/lib.rs",
            "pub fn ok() {}\n\n#[cfg(target_os = \"linux\")]\nfn legacy() -> &'static str { \"com.tatolab.foo\" }\n",
        );
        let violations = lint_workspace(tmp.path()).unwrap();
        assert_eq!(violations.len(), 1, "got {violations:?}");
    }
}
