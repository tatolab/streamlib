// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Lints ad-hoc logging patterns in the Rust workspace and in the Python /
//! Python polyglot SDKs. The only sanctioned pathway is `tracing::*` /
//! `streamlib.log.*` — see `docs/logging.md`.
//!
//! Python uses a ripgrep-style substring scan. Rust uses a `syn`
//! AST walk so that `#[cfg(test)]`, `#[allow(clippy::disallowed_macros)]`, and
//! file-level `#![allow(...)]` are honored exactly as clippy would — without
//! having to compile the workspace (which would pull in `glslc` via
//! `runtime/streamlib-engine`'s build script).

use anyhow::{Context, Result};
use std::fs;
use std::path::{Path, PathBuf};
use syn::visit::Visit;
use walkdir::WalkDir;

pub struct LintTarget {
    pub name: &'static str,
    pub root_relative: &'static str,
    pub extension: &'static str,
    pub exclude_path_segments: &'static [&'static str],
    pub exclude_file_suffixes: &'static [&'static str],
    pub comment_prefix: &'static str,
    pub banned_substrings: &'static [&'static str],
    pub allow_substring: &'static str,
}

/// One policy, applied to every Python root the engine tree owns. A second
/// literal would let the wheel's package silently lint less than the SDK's the
/// first time the ban list or the exclusions change.
const fn python_logging_lint_target(name: &'static str, root_relative: &'static str) -> LintTarget {
    LintTarget {
        name,
        root_relative,
        extension: "py",
        exclude_path_segments: &[
            "tests",
            "_generated_",
            "__pycache__",
            ".venv",
            "build",
            "dist",
        ],
        exclude_file_suffixes: &["_test.py"],
        comment_prefix: "#",
        banned_substrings: &["print(", "sys.stdout", "sys.stderr", "logging.basicConfig"],
        allow_substring: "streamlib.log.",
    }
}

pub const TARGETS: &[LintTarget] = &[python_logging_lint_target(
    "python",
    "sdk/streamlib-python-wheel/python",
)];

#[derive(Debug)]
pub struct Violation {
    pub path: PathBuf,
    pub line_no: usize,
    pub line_text: String,
    pub matched_pattern: &'static str,
    pub target: &'static str,
}

/// How many files one scan root contributed to a lint run.
///
/// Per-root rather than one workspace-wide total: this lint reads the
/// Python wheel tree and every opted-in Rust crate's source roots, and a
/// single sum lets one tree going to zero hide behind the others' surviving
/// files.
#[derive(Debug)]
pub struct LintLoggingScanRootFileCount {
    pub scan_root_description: String,
    pub files_scanned: usize,
}

pub struct LintReport {
    pub violations: Vec<Violation>,
    pub scan_root_file_counts: Vec<LintLoggingScanRootFileCount>,
}

impl LintReport {
    pub fn files_scanned(&self) -> usize {
        self.scan_root_file_counts
            .iter()
            .map(|count| count.files_scanned)
            .sum()
    }
}

pub fn run(project_root: &Path) -> Result<()> {
    let report = scan_all(project_root)?;
    for v in &report.violations {
        eprintln!(
            "{}:{}: [{}] banned `{}` — use tracing::* / streamlib.log.* / see docs/logging.md\n    {}",
            v.path.display(),
            v.line_no,
            v.target,
            v.matched_pattern,
            v.line_text.trim_end(),
        );
    }
    if report.violations.is_empty() {
        for count in &report.scan_root_file_counts {
            crate::ensure_source_walking_gate_read_source(
                "lint-logging",
                &count.scan_root_description,
                count.files_scanned,
                "a banned print macro back in",
            )?;
        }
        println!(
            "lint-logging: {} file(s) scanned across Rust + polyglot SDKs, no violations",
            report.files_scanned(),
        );
        Ok(())
    } else {
        Err(anyhow::anyhow!(
            "lint-logging: {} violation(s) across {} file(s) scanned",
            report.violations.len(),
            report.files_scanned(),
        ))
    }
}

pub fn scan_all(project_root: &Path) -> Result<LintReport> {
    let mut violations = Vec::new();
    let mut scan_root_file_counts = Vec::new();
    for target in TARGETS {
        let root = project_root.join(target.root_relative);
        let mut files_scanned = 0usize;
        if root.exists() {
            scan_target(&root, target, &mut violations, &mut files_scanned)?;
        }
        scan_root_file_counts.push(LintLoggingScanRootFileCount {
            scan_root_description: format!(
                "the {} scan root {}",
                target.name, target.root_relative
            ),
            files_scanned,
        });
    }
    scan_root_file_counts.extend(scan_rust(project_root, &mut violations)?);
    Ok(LintReport {
        violations,
        scan_root_file_counts,
    })
}

fn scan_target(
    root: &Path,
    target: &'static LintTarget,
    violations: &mut Vec<Violation>,
    files_scanned: &mut usize,
) -> Result<()> {
    for entry in WalkDir::new(root).into_iter().filter_map(|e| e.ok()) {
        let path = entry.path();
        if !entry.file_type().is_file() {
            continue;
        }
        if path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e != target.extension)
            .unwrap_or(true)
        {
            continue;
        }
        if is_excluded(path, root, target) {
            continue;
        }
        *files_scanned += 1;
        scan_file(path, target, violations)?;
    }
    Ok(())
}

fn is_excluded(path: &Path, root: &Path, target: &LintTarget) -> bool {
    let rel = path.strip_prefix(root).unwrap_or(path);
    for component in rel.components() {
        if let std::path::Component::Normal(seg) = component {
            if let Some(seg_str) = seg.to_str() {
                if target.exclude_path_segments.contains(&seg_str) {
                    return true;
                }
            }
        }
    }
    if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
        for suffix in target.exclude_file_suffixes {
            if name.ends_with(suffix) {
                return true;
            }
        }
    }
    false
}

/// Marker comment that exempts an entire file. Used by the infrastructure
/// files that *install* the unified pathway — `_log_interceptors.py` and
/// the subprocess bootstrap runners that must emit diagnostics before the
/// logging pipeline is wired.
const ALLOW_FILE_PRAGMA: &str = "streamlib:lint-logging:allow-file";

/// Marker comment that exempts a single line.
const ALLOW_LINE_PRAGMA: &str = "streamlib:lint-logging:allow-line";

fn scan_file(
    path: &Path,
    target: &'static LintTarget,
    violations: &mut Vec<Violation>,
) -> Result<()> {
    let content =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    if content.contains(ALLOW_FILE_PRAGMA) {
        return Ok(());
    }
    for (idx, line) in content.lines().enumerate() {
        if is_comment_line(line, target.comment_prefix) {
            continue;
        }
        if line.contains(target.allow_substring) {
            continue;
        }
        if line.contains(ALLOW_LINE_PRAGMA) {
            continue;
        }
        for pattern in target.banned_substrings {
            if line.contains(pattern) {
                violations.push(Violation {
                    path: path.to_path_buf(),
                    line_no: idx + 1,
                    line_text: line.to_string(),
                    matched_pattern: pattern,
                    target: target.name,
                });
                break;
            }
        }
    }
    Ok(())
}

fn is_comment_line(line: &str, prefix: &str) -> bool {
    line.trim_start().starts_with(prefix)
}

// ---------------------------------------------------------------------------
// Rust target — AST-based, not substring
// ---------------------------------------------------------------------------

/// Macro paths that must not appear in library / bin code of opt-in crates.
/// Mirrors the `disallowed-macros` list in `clippy.toml`.
const RUST_BANNED_MACROS: &[(&str, &str)] = &[
    ("println", "println!"),
    ("eprintln", "eprintln!"),
    ("print", "print!"),
    ("eprint", "eprint!"),
    ("dbg", "dbg!"),
];

/// Walks every crate that opts into workspace lints
/// (`[lints] workspace = true` in its Cargo.toml) and checks each `.rs` file
/// under its source roots for banned macro invocations. Crates that don't opt
/// in (CLI binaries, runtime binaries) are out of the lockout by design.
///
/// Returns one file count per source root name rather than a single total, so
/// [`run`] can refuse a run in which one root read nothing.
///
/// Respects `#[cfg(...)]` on out-of-line mod declarations in the crate root
/// (e.g. `#[cfg(target_os = "macos")] mod apple;`) so that files the Linux
/// runner's clippy would never parse are also skipped here.
pub fn scan_rust(
    project_root: &Path,
    violations: &mut Vec<Violation>,
) -> Result<Vec<LintLoggingScanRootFileCount>> {
    let mut files_scanned_per_source_root =
        vec![0usize; crate::RUST_CRATE_SOURCE_ROOT_DIR_NAMES.len()];
    for crate_root in discover_lint_opted_in_crates(project_root)? {
        let excluded = collect_cfg_excluded_mod_paths(&crate_root);
        for (root_index, root_name) in crate::RUST_CRATE_SOURCE_ROOT_DIR_NAMES.iter().enumerate() {
            let files_scanned = &mut files_scanned_per_source_root[root_index];
            let source_root = crate_root.join(root_name);
            if !source_root.exists() {
                continue;
            }
            for entry in WalkDir::new(&source_root)
                .into_iter()
                .filter_map(|e| e.ok())
            {
                let path = entry.path();
                if !entry.file_type().is_file() {
                    continue;
                }
                if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                    continue;
                }
                if excluded.iter().any(|p| path.starts_with(p)) {
                    continue;
                }
                if path.components().any(|c| {
                    c.as_os_str()
                        .to_str()
                        .is_some_and(is_parked_pending_segment)
                }) {
                    // Parked implementation dirs (`_apple_impl_pending_`,
                    // `_nvjpeg_impl_pending_`, ...) hold code that is NOT
                    // declared in any mod graph and never compiles — lint it
                    // when it is activated, not while parked.
                    continue;
                }
                *files_scanned += 1;
                scan_rust_file(path, violations)?;
            }
        }
    }
    Ok(crate::RUST_CRATE_SOURCE_ROOT_DIR_NAMES
        .iter()
        .zip(files_scanned_per_source_root)
        .map(|(root_name, files_scanned)| LintLoggingScanRootFileCount {
            scan_root_description: format!(
                "every lint-opted-in Rust crate's `{root_name}/` source root"
            ),
            files_scanned,
        })
        .collect())
}

/// Whether a path segment names a parked implementation dir per the
/// `_<name>_pending_` convention (`_apple_impl_pending_`,
/// `_nvjpeg_impl_pending_`, `tests_pending` nested inside one, ...).
fn is_parked_pending_segment(segment: &str) -> bool {
    segment.starts_with('_') && segment.ends_with("pending_")
}

/// Starting at the crate's module roots, walk out-of-line `mod foo;`
/// declarations. Any mod whose `#[cfg(...)]` attribute evaluates to false on
/// `ubuntu-latest` contributes its source path (file and/or directory) to the
/// exclusion set; other mods are recursed into so deeper cfg-gated mods are
/// caught too.
///
/// A crate that commits `src/lib.rs` is walked from there. A folder-backed
/// A folder-backed crate reaches its `processors/` files through `#[path]`
/// arms in its committed `src/lib.rs`, which [`resolve_mod_candidates`]
/// honours, so one walk from the crate root covers both shapes.
fn collect_cfg_excluded_mod_paths(crate_root: &Path) -> Vec<PathBuf> {
    let mut excluded = Vec::new();
    let lib_rs = crate_root.join("src/lib.rs");
    if lib_rs.exists() {
        walk_mods_for_exclusions(&lib_rs, &mut excluded);
    }
    excluded
}

fn walk_mods_for_exclusions(file_path: &Path, excluded: &mut Vec<PathBuf>) {
    let Ok(content) = fs::read_to_string(file_path) else {
        return;
    };
    let Ok(file) = syn::parse_file(&content) else {
        return;
    };
    // A file-level `#![cfg(...)]` gates the module the file IS, so a predicate
    // false on the runner strips the file and everything it declares.
    if file.attrs.iter().any(is_cfg_excluded_on_linux) {
        let stem = file_path
            .file_stem()
            .and_then(|n| n.to_str())
            .unwrap_or_default()
            .to_string();
        push_mod_exclusion(file_path, &stem, excluded);
        return;
    }
    for item in &file.items {
        if let syn::Item::Mod(m) = item {
            // Only out-of-line mods (`mod foo;`); inline `mod foo { ... }` is
            // linted through the normal AST walk.
            if m.content.is_some() {
                continue;
            }
            let mod_name = m.ident.to_string();
            let mod_parent = file_path.parent().unwrap_or(Path::new(""));
            let candidates = resolve_mod_candidates(mod_parent, &mod_name, &m.attrs);
            let Some(found) = candidates.into_iter().find(|p| p.exists()) else {
                continue;
            };
            let gated_out = m.attrs.iter().any(is_cfg_excluded_on_linux);
            if gated_out {
                push_mod_exclusion(&found, &mod_name, excluded);
            } else {
                walk_mods_for_exclusions(&found, excluded);
            }
        }
    }
}

fn resolve_mod_candidates(
    parent_dir: &Path,
    mod_name: &str,
    attrs: &[syn::Attribute],
) -> Vec<PathBuf> {
    // #[path = "custom.rs"] mod foo; — honor the override.
    for attr in attrs {
        if attr.path().is_ident("path") {
            if let syn::Meta::NameValue(nv) = &attr.meta {
                if let syn::Expr::Lit(syn::ExprLit {
                    lit: syn::Lit::Str(s),
                    ..
                }) = &nv.value
                {
                    return vec![parent_dir.join(s.value())];
                }
            }
        }
    }
    vec![
        parent_dir.join(format!("{mod_name}.rs")),
        parent_dir.join(mod_name).join("mod.rs"),
    ]
}

fn push_mod_exclusion(found: &Path, mod_name: &str, excluded: &mut Vec<PathBuf>) {
    let is_mod_rs = found.file_name().and_then(|n| n.to_str()) == Some("mod.rs");
    if is_mod_rs {
        if let Some(dir) = found.parent() {
            excluded.push(dir.to_path_buf());
        }
        return;
    }
    excluded.push(found.to_path_buf());
    // A `foo.rs` mod may have sub-modules at `foo/bar.rs` — exclude that dir too.
    if let Some(parent) = found.parent() {
        let sibling = parent.join(mod_name);
        if sibling.exists() {
            excluded.push(sibling);
        }
    }
}

fn discover_lint_opted_in_crates(project_root: &Path) -> Result<Vec<PathBuf>> {
    // `packages` carries the only in-tree crates rooted at `processors/`. Left
    // out, this gate declares a source root nothing under it can ever hold, and
    // the per-root refusal in `run` has no way to tell "the root moved" from
    // "this gate never reads that root".
    const ZONE_PARENTS: &[&str] = &["runtime", "sdk", "adapters", "vendor", "packages"];
    let mut result = Vec::new();
    for zone in ZONE_PARENTS {
        let zone_dir = project_root.join(zone);
        if !zone_dir.exists() {
            continue;
        }
        for entry in
            fs::read_dir(&zone_dir).with_context(|| format!("read_dir {}", zone_dir.display()))?
        {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let cargo_toml = entry.path().join("Cargo.toml");
            if !cargo_toml.exists() {
                continue;
            }
            let content = fs::read_to_string(&cargo_toml)
                .with_context(|| format!("read {}", cargo_toml.display()))?;
            let parsed: toml::Value = match toml::from_str(&content) {
                Ok(v) => v,
                Err(_) => continue,
            };
            let opts_in = parsed
                .get("lints")
                .and_then(|v| v.get("workspace"))
                .and_then(|v| v.as_bool())
                == Some(true);
            if opts_in {
                result.push(entry.path());
            }
        }
    }
    result.sort();
    Ok(result)
}

fn scan_rust_file(path: &Path, violations: &mut Vec<Violation>) -> Result<()> {
    let content =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    if content.contains(ALLOW_FILE_PRAGMA) {
        return Ok(());
    }
    let file = match syn::parse_file(&content) {
        Ok(f) => f,
        Err(_) => return Ok(()), // unparseable file — leave to rustc/clippy to surface
    };
    // File-level #![allow(clippy::disallowed_macros)] or any inner #![cfg(test)]
    // guard (including `cfg(all(test, ...))`) skips the whole file.
    if file.attrs.iter().any(is_skip_attr) {
        return Ok(());
    }
    let source_lines: Vec<&str> = content.lines().collect();
    let mut visitor = RustVisitor {
        path,
        source_lines: &source_lines,
        violations,
    };
    visitor.visit_file(&file);
    Ok(())
}

struct RustVisitor<'a> {
    path: &'a Path,
    source_lines: &'a [&'a str],
    violations: &'a mut Vec<Violation>,
}

impl<'ast, 'a> Visit<'ast> for RustVisitor<'a> {
    fn visit_item(&mut self, item: &'ast syn::Item) {
        if let Some(attrs) = item_attrs(item) {
            if attrs.iter().any(is_skip_attr) {
                return;
            }
        }
        syn::visit::visit_item(self, item);
    }

    fn visit_expr(&mut self, expr: &'ast syn::Expr) {
        if let Some(attrs) = expr_attrs(expr) {
            if attrs.iter().any(is_skip_attr) {
                return;
            }
        }
        if let syn::Expr::Macro(m) = expr {
            self.check_macro(&m.mac);
        }
        syn::visit::visit_expr(self, expr);
    }

    fn visit_stmt(&mut self, stmt: &'ast syn::Stmt) {
        if let syn::Stmt::Macro(m) = stmt {
            if !m.attrs.iter().any(is_skip_attr) {
                self.check_macro(&m.mac);
            }
            // don't descend — no inner exprs to visit in a stmt-macro
            return;
        }
        syn::visit::visit_stmt(self, stmt);
    }

    fn visit_item_macro(&mut self, item: &'ast syn::ItemMacro) {
        if !item.attrs.iter().any(is_skip_attr) {
            self.check_macro(&item.mac);
        }
    }

    fn visit_impl_item(&mut self, item: &'ast syn::ImplItem) {
        let attrs: &[syn::Attribute] = match item {
            syn::ImplItem::Const(x) => &x.attrs,
            syn::ImplItem::Fn(x) => &x.attrs,
            syn::ImplItem::Type(x) => &x.attrs,
            syn::ImplItem::Macro(x) => &x.attrs,
            _ => &[],
        };
        if attrs.iter().any(is_skip_attr) {
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
        if attrs.iter().any(is_skip_attr) {
            return;
        }
        syn::visit::visit_trait_item(self, item);
    }

    fn visit_foreign_item(&mut self, item: &'ast syn::ForeignItem) {
        let attrs: &[syn::Attribute] = match item {
            syn::ForeignItem::Fn(x) => &x.attrs,
            syn::ForeignItem::Static(x) => &x.attrs,
            syn::ForeignItem::Type(x) => &x.attrs,
            syn::ForeignItem::Macro(x) => &x.attrs,
            _ => &[],
        };
        if attrs.iter().any(is_skip_attr) {
            return;
        }
        syn::visit::visit_foreign_item(self, item);
    }
}

impl<'a> RustVisitor<'a> {
    fn check_macro(&mut self, mac: &syn::Macro) {
        let Some(last) = mac.path.segments.last() else {
            return;
        };
        // Be strict about the path: accept bare `println!` or `std::println!`.
        // Anything else is not the std macro we're banning.
        let is_std_or_bare = match mac.path.segments.len() {
            1 => true,
            2 => mac
                .path
                .segments
                .first()
                .map(|s| s.ident == "std")
                .unwrap_or(false),
            _ => false,
        };
        if !is_std_or_bare {
            return;
        }
        let name = last.ident.to_string();
        let Some(&(_, display)) = RUST_BANNED_MACROS.iter().find(|(n, _)| *n == name) else {
            return;
        };
        let line_no = last.ident.span().start().line;
        let line_text = self
            .source_lines
            .get(line_no.saturating_sub(1))
            .copied()
            .unwrap_or("")
            .to_string();
        if line_text.contains(ALLOW_LINE_PRAGMA) {
            return;
        }
        self.violations.push(Violation {
            path: self.path.to_path_buf(),
            line_no,
            line_text,
            matched_pattern: display,
            target: "rust",
        });
    }
}

fn is_skip_attr(attr: &syn::Attribute) -> bool {
    is_cfg_excluded_on_linux(attr) || is_allow_disallowed_macros(attr)
}

/// True if this `#[cfg(...)]` attribute guards an item that clippy running on
/// `cargo clippy --workspace --no-deps` on `ubuntu-latest` would NOT see.
/// Mirrors the runner's cfg state: linux / unix, no `test`, no `debug_assertions`
/// treated as set (conservative "unknown → included").
fn is_cfg_excluded_on_linux(attr: &syn::Attribute) -> bool {
    if !attr.path().is_ident("cfg") {
        return false;
    }
    let Ok(tokens) = attr.meta.require_list().map(|l| l.tokens.clone()) else {
        return false;
    };
    eval_cfg(tokens) == CfgEval::False
}

#[derive(PartialEq, Eq, Clone, Copy)]
enum CfgEval {
    True,
    False,
    /// Unknown predicate (feature flags, target_arch, etc.) — treat as "item
    /// may be present", i.e., lint conservatively.
    Unknown,
}

fn eval_cfg(tokens: proc_macro2::TokenStream) -> CfgEval {
    let mut iter = tokens.into_iter().peekable();
    eval_predicate(&mut iter)
}

fn eval_predicate(iter: &mut std::iter::Peekable<proc_macro2::token_stream::IntoIter>) -> CfgEval {
    let Some(tt) = iter.next() else {
        return CfgEval::Unknown;
    };
    match tt {
        proc_macro2::TokenTree::Ident(ident) => {
            let name = ident.to_string();
            // Peek: combinator (all/any/not) is followed by a Group.
            match (name.as_str(), iter.peek()) {
                ("all", Some(proc_macro2::TokenTree::Group(_))) => {
                    let Some(proc_macro2::TokenTree::Group(g)) = iter.next() else {
                        unreachable!()
                    };
                    eval_all(g.stream())
                }
                ("any", Some(proc_macro2::TokenTree::Group(_))) => {
                    let Some(proc_macro2::TokenTree::Group(g)) = iter.next() else {
                        unreachable!()
                    };
                    eval_any(g.stream())
                }
                ("not", Some(proc_macro2::TokenTree::Group(_))) => {
                    let Some(proc_macro2::TokenTree::Group(g)) = iter.next() else {
                        unreachable!()
                    };
                    let mut inner = g.stream().into_iter().peekable();
                    match eval_predicate(&mut inner) {
                        CfgEval::True => CfgEval::False,
                        CfgEval::False => CfgEval::True,
                        CfgEval::Unknown => CfgEval::Unknown,
                    }
                }
                // Bare `cfg(test)` — single identifier, no group follows, or
                // the next tokens are not a group (e.g., comma).
                _ => eval_bare(&name, iter),
            }
        }
        proc_macro2::TokenTree::Group(g) => {
            // Parenthesized predicate: eval its stream.
            let mut inner = g.stream().into_iter().peekable();
            eval_predicate(&mut inner)
        }
        _ => CfgEval::Unknown,
    }
}

/// Evaluate a bare identifier predicate — either a flag (`test`,
/// `debug_assertions`) or a name-value pair (`target_os = "linux"`).
fn eval_bare(
    name: &str,
    iter: &mut std::iter::Peekable<proc_macro2::token_stream::IntoIter>,
) -> CfgEval {
    // Look ahead for `= "value"` — a Punct('=') then a Literal.
    if let Some(proc_macro2::TokenTree::Punct(p)) = iter.peek() {
        if p.as_char() == '=' {
            iter.next();
            if let Some(proc_macro2::TokenTree::Literal(lit)) = iter.next() {
                let value_raw = lit.to_string();
                let value = value_raw.trim_matches('"');
                return eval_name_value(name, value);
            }
            return CfgEval::Unknown;
        }
    }
    match name {
        "test" => CfgEval::False,
        "unix" => CfgEval::True,
        "windows" => CfgEval::False,
        // Assume default rustflags: debug_assertions is set for dev profile,
        // which is what `cargo clippy` uses.
        "debug_assertions" => CfgEval::True,
        _ => CfgEval::Unknown,
    }
}

fn eval_name_value(name: &str, value: &str) -> CfgEval {
    match name {
        "target_os" => {
            if value == "linux" {
                CfgEval::True
            } else {
                CfgEval::False
            }
        }
        "target_family" => match value {
            "unix" => CfgEval::True,
            "windows" | "wasm" => CfgEval::False,
            _ => CfgEval::Unknown,
        },
        "target_env" => match value {
            "gnu" => CfgEval::True,
            "msvc" | "musl" => CfgEval::False,
            _ => CfgEval::Unknown,
        },
        "target_vendor" => match value {
            "unknown" => CfgEval::True,
            "apple" | "pc" => CfgEval::False,
            _ => CfgEval::Unknown,
        },
        // target_arch, feature flags, rustc flags we don't track — conservative.
        _ => CfgEval::Unknown,
    }
}

fn eval_all(stream: proc_macro2::TokenStream) -> CfgEval {
    let predicates = split_on_commas(stream);
    let mut any_unknown = false;
    for p in predicates {
        let mut iter = p.into_iter().peekable();
        match eval_predicate(&mut iter) {
            CfgEval::False => return CfgEval::False,
            CfgEval::Unknown => any_unknown = true,
            CfgEval::True => {}
        }
    }
    if any_unknown {
        CfgEval::Unknown
    } else {
        CfgEval::True
    }
}

fn eval_any(stream: proc_macro2::TokenStream) -> CfgEval {
    let predicates = split_on_commas(stream);
    let mut any_unknown = false;
    for p in predicates {
        let mut iter = p.into_iter().peekable();
        match eval_predicate(&mut iter) {
            CfgEval::True => return CfgEval::True,
            CfgEval::Unknown => any_unknown = true,
            CfgEval::False => {}
        }
    }
    if any_unknown {
        CfgEval::Unknown
    } else {
        CfgEval::False
    }
}

fn split_on_commas(stream: proc_macro2::TokenStream) -> Vec<proc_macro2::TokenStream> {
    let mut out: Vec<proc_macro2::TokenStream> = Vec::new();
    let mut current: Vec<proc_macro2::TokenTree> = Vec::new();
    for tt in stream {
        if let proc_macro2::TokenTree::Punct(ref p) = tt {
            if p.as_char() == ',' {
                out.push(current.drain(..).collect());
                continue;
            }
        }
        current.push(tt);
    }
    if !current.is_empty() {
        out.push(current.into_iter().collect());
    }
    out
}

fn is_allow_disallowed_macros(attr: &syn::Attribute) -> bool {
    // Matches both `#[allow(clippy::disallowed_macros)]` and
    // `#[expect(clippy::disallowed_macros)]`.
    if !(attr.path().is_ident("allow") || attr.path().is_ident("expect")) {
        return false;
    }
    let mut hit = false;
    let _ = attr.parse_nested_meta(|meta| {
        let segs: Vec<_> = meta
            .path
            .segments
            .iter()
            .map(|s| s.ident.to_string())
            .collect();
        if segs == ["clippy", "disallowed_macros"] {
            hit = true;
        }
        Ok(())
    });
    hit
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
        syn::Expr::Assign(x) => &x.attrs,
        syn::Expr::Async(x) => &x.attrs,
        syn::Expr::Await(x) => &x.attrs,
        syn::Expr::Binary(x) => &x.attrs,
        syn::Expr::Block(x) => &x.attrs,
        syn::Expr::Break(x) => &x.attrs,
        syn::Expr::Call(x) => &x.attrs,
        syn::Expr::Cast(x) => &x.attrs,
        syn::Expr::Closure(x) => &x.attrs,
        syn::Expr::Const(x) => &x.attrs,
        syn::Expr::Continue(x) => &x.attrs,
        syn::Expr::Field(x) => &x.attrs,
        syn::Expr::ForLoop(x) => &x.attrs,
        syn::Expr::Group(x) => &x.attrs,
        syn::Expr::If(x) => &x.attrs,
        syn::Expr::Index(x) => &x.attrs,
        syn::Expr::Infer(x) => &x.attrs,
        syn::Expr::Let(x) => &x.attrs,
        syn::Expr::Lit(x) => &x.attrs,
        syn::Expr::Loop(x) => &x.attrs,
        syn::Expr::Macro(x) => &x.attrs,
        syn::Expr::Match(x) => &x.attrs,
        syn::Expr::MethodCall(x) => &x.attrs,
        syn::Expr::Paren(x) => &x.attrs,
        syn::Expr::Path(x) => &x.attrs,
        syn::Expr::Range(x) => &x.attrs,
        syn::Expr::RawAddr(x) => &x.attrs,
        syn::Expr::Reference(x) => &x.attrs,
        syn::Expr::Repeat(x) => &x.attrs,
        syn::Expr::Return(x) => &x.attrs,
        syn::Expr::Struct(x) => &x.attrs,
        syn::Expr::Try(x) => &x.attrs,
        syn::Expr::TryBlock(x) => &x.attrs,
        syn::Expr::Tuple(x) => &x.attrs,
        syn::Expr::Unary(x) => &x.attrs,
        syn::Expr::Unsafe(x) => &x.attrs,
        syn::Expr::While(x) => &x.attrs,
        syn::Expr::Yield(x) => &x.attrs,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn write_fixture(dir: &Path, rel: &str, content: &str) -> PathBuf {
        let path = dir.join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&path, content).unwrap();
        path
    }

    fn scan_fixture_tree(
        target: &'static LintTarget,
        content: &str,
        rel_path: &str,
    ) -> Vec<Violation> {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().to_path_buf();
        write_fixture(&root, rel_path, content);
        let mut violations = Vec::new();
        let mut files_scanned = 0usize;
        scan_target(&root, target, &mut violations, &mut files_scanned).unwrap();
        violations
    }

    /// Resolved by name, not position: `TARGETS` gains entries over time, and
    /// an index would silently retarget every assertion below when it does.
    fn lint_target_named(name: &str) -> &'static LintTarget {
        TARGETS
            .iter()
            .find(|target| target.name == name)
            .unwrap_or_else(|| panic!("no lint target named {name}"))
    }

    #[test]
    fn rejects_print_in_python_library() {
        let v = scan_fixture_tree(
            lint_target_named("python"),
            "print(\"hello\")\n",
            "src/app.py",
        );
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].matched_pattern, "print(");
    }

    #[test]
    fn rejects_sys_stdout_in_python() {
        let v = scan_fixture_tree(
            lint_target_named("python"),
            "import sys\nsys.stdout.write(\"hi\")\n",
            "src/app.py",
        );
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].matched_pattern, "sys.stdout");
    }

    #[test]
    fn rejects_logging_basicconfig_in_python() {
        let v = scan_fixture_tree(
            lint_target_named("python"),
            "import logging\nlogging.basicConfig(level=logging.INFO)\n",
            "src/app.py",
        );
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].matched_pattern, "logging.basicConfig");
    }

    #[test]
    fn accepts_streamlib_log_python() {
        let v = scan_fixture_tree(
            lint_target_named("python"),
            "import streamlib\nstreamlib.log.info(\"hi\")\n",
            "src/app.py",
        );
        assert!(v.is_empty(), "streamlib.log.* should pass: {:?}", v);
    }

    #[test]
    fn skips_comment_lines_python() {
        let v = scan_fixture_tree(
            lint_target_named("python"),
            "# don't use print(\"x\") here\nstreamlib.log.info(\"ok\")\n",
            "src/app.py",
        );
        assert!(v.is_empty(), "commented-out print should not flag: {:?}", v);
    }

    #[test]
    fn excludes_tests_directory_python() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().to_path_buf();
        write_fixture(&root, "tests/test_app.py", "print(\"hi\")\n");
        write_fixture(&root, "src/app.py", "streamlib.log.info(\"ok\")\n");
        let mut violations = Vec::new();
        let mut files_scanned = 0usize;
        scan_target(
            &root,
            lint_target_named("python"),
            &mut violations,
            &mut files_scanned,
        )
        .unwrap();
        assert!(
            violations.is_empty(),
            "tests/ should be excluded: {:?}",
            violations
        );
    }

    #[test]
    fn allow_file_pragma_skips_entire_file_python() {
        let v = scan_fixture_tree(
            lint_target_named("python"),
            "# streamlib:lint-logging:allow-file — this is the interceptor installer itself\nimport sys\nsys.stdout = MyInterceptor()\n",
            "src/_log_interceptors.py",
        );
        assert!(
            v.is_empty(),
            "allow-file pragma should suppress entire file: {:?}",
            v
        );
    }

    #[test]
    fn allow_line_pragma_skips_single_line_python() {
        let v = scan_fixture_tree(
            lint_target_named("python"),
            "sys.stderr.write(\"fallback\")  # streamlib:lint-logging:allow-line\nprint(\"bad\")\n",
            "src/app.py",
        );
        assert_eq!(v.len(), 1, "only the non-allowed line should flag: {:?}", v);
        assert_eq!(v[0].matched_pattern, "print(");
    }

    // ----- Rust target -------------------------------------------------------

    fn scan_rust_source(content: &str) -> Vec<Violation> {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("probe.rs");
        fs::write(&path, content).unwrap();
        let mut violations = Vec::new();
        scan_rust_file(&path, &mut violations).unwrap();
        violations
    }

    #[test]
    fn rust_rejects_plain_println() {
        let v = scan_rust_source("pub fn f() { println!(\"x\"); }\n");
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].matched_pattern, "println!");
        assert_eq!(v[0].target, "rust");
    }

    #[test]
    fn rust_rejects_all_five_banned_macros() {
        for (mac, display) in &[
            ("println!(\"x\")", "println!"),
            ("eprintln!(\"x\")", "eprintln!"),
            ("print!(\"x\")", "print!"),
            ("eprint!(\"x\")", "eprint!"),
            ("dbg!(1)", "dbg!"),
        ] {
            let src = format!("pub fn f() {{ {}; }}\n", mac);
            let v = scan_rust_source(&src);
            assert_eq!(v.len(), 1, "{} should be rejected", mac);
            assert_eq!(v[0].matched_pattern, *display);
        }
    }

    #[test]
    fn rust_accepts_tracing_macros() {
        let v = scan_rust_source("pub fn f() { tracing::info!(\"x\"); }\n");
        assert!(v.is_empty(), "tracing::* should pass: {:?}", v);
    }

    #[test]
    fn rust_file_level_allow_skips_entire_file() {
        let v = scan_rust_source(
            "#![allow(clippy::disallowed_macros)]\npub fn f() { println!(\"x\"); }\n",
        );
        assert!(v.is_empty(), "file-level allow should suppress: {:?}", v);
    }

    #[test]
    fn rust_file_level_custom_pragma_skips_file() {
        let v = scan_rust_source(
            "// streamlib:lint-logging:allow-file\npub fn f() { println!(\"x\"); }\n",
        );
        assert!(v.is_empty(), "allow-file pragma should suppress: {:?}", v);
    }

    #[test]
    fn rust_inline_allow_on_fn_suppresses_body() {
        let v = scan_rust_source(
            "#[allow(clippy::disallowed_macros)]\npub fn f() { println!(\"x\"); }\n",
        );
        assert!(v.is_empty(), "fn-level allow should suppress body: {:?}", v);
    }

    #[test]
    fn rust_inline_allow_on_impl_fn_suppresses_body() {
        let v = scan_rust_source(
            "struct W;\nimpl W {\n  #[allow(clippy::disallowed_macros)]\n  pub fn f(&self) { println!(\"x\"); }\n}\n",
        );
        assert!(v.is_empty(), "impl-fn-level allow should suppress: {:?}", v);
    }

    #[test]
    fn rust_inline_allow_on_block_expr_suppresses() {
        // Matches the pattern in libs/streamlib/src/core/logging/init.rs.
        let v = scan_rust_source(
            "pub fn f() {\n  #[allow(clippy::disallowed_macros)]\n  {\n    eprintln!(\"x\");\n  }\n}\n",
        );
        assert!(v.is_empty(), "block-expr allow should suppress: {:?}", v);
    }

    #[test]
    fn rust_cfg_test_skips_mod() {
        let v = scan_rust_source("#[cfg(test)]\nmod tests {\n  fn t() { println!(\"x\"); }\n}\n");
        assert!(v.is_empty(), "#[cfg(test)] mod should be skipped: {:?}", v);
    }

    #[test]
    fn rust_cfg_all_test_and_linux_skips_file() {
        // Mirrors src/vulkan/rhi/vulkan_swapchain_dma_buf_allocation_fix_validation_test.rs.
        let v = scan_rust_source(
            "#![cfg(all(test, target_os = \"linux\"))]\npub fn f() { println!(\"x\"); }\n",
        );
        assert!(v.is_empty(), "cfg(all(test,...)) should skip file: {:?}", v);
    }

    #[test]
    fn rust_cfg_macos_skips_item() {
        let v =
            scan_rust_source("#[cfg(target_os = \"macos\")]\npub fn mac() { println!(\"x\"); }\n");
        assert!(
            v.is_empty(),
            "target_os=macos should be skipped on linux: {:?}",
            v
        );
    }

    #[test]
    fn rust_cfg_any_macos_or_linux_lints_item() {
        let v = scan_rust_source(
            "#[cfg(any(target_os = \"macos\", target_os = \"linux\"))]\npub fn cross() { println!(\"x\"); }\n",
        );
        assert_eq!(
            v.len(),
            1,
            "cfg(any(macos, linux)) is included on linux, should lint: {:?}",
            v
        );
    }

    #[test]
    fn rust_cfg_not_windows_lints_item() {
        let v = scan_rust_source(
            "#[cfg(not(target_os = \"windows\"))]\npub fn nw() { println!(\"x\"); }\n",
        );
        assert_eq!(
            v.len(),
            1,
            "cfg(not(windows)) is true on linux, should lint: {:?}",
            v
        );
    }

    #[test]
    fn rust_feature_cfg_is_conservative_and_lints() {
        // Unknown predicates (feature flags) → linted conservatively.
        let v = scan_rust_source(
            "#[cfg(feature = \"debug-overlay\")]\npub fn d() { println!(\"x\"); }\n",
        );
        assert_eq!(
            v.len(),
            1,
            "feature cfg should lint conservatively: {:?}",
            v
        );
    }

    #[test]
    fn rust_qualified_std_println_is_rejected() {
        let v = scan_rust_source("pub fn f() { std::println!(\"x\"); }\n");
        assert_eq!(v.len(), 1, "std::println! should be rejected: {:?}", v);
    }

    #[test]
    fn rust_non_std_similarly_named_macro_is_ignored() {
        // A user-defined `my_crate::println!` is not what the lockout bans.
        let v = scan_rust_source("pub fn f() { my_crate::println!(\"x\"); }\n");
        assert!(
            v.is_empty(),
            "non-std path-qualified macro with same tail name should not fire: {:?}",
            v
        );
    }

    #[test]
    fn rust_allow_line_pragma_skips_single_macro() {
        let v = scan_rust_source(
            "pub fn f() {\n  println!(\"x\"); // streamlib:lint-logging:allow-line\n  println!(\"bad\");\n}\n",
        );
        assert_eq!(v.len(), 1, "only the non-allowed line should fire: {:?}", v);
        assert!(v[0].line_text.contains("bad"));
    }

    #[test]
    fn rust_out_of_line_mod_cfg_excludes_subtree() {
        // Emulates libs/streamlib/src/lib.rs: `#[cfg(target_os = "macos")] pub mod apple;`
        // pointing at `apple/mod.rs` which contains a banned macro.
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let crate_root = root.join("sdk/streamlib-macros");
        fs::create_dir_all(crate_root.join("src/apple")).unwrap();
        fs::write(
            crate_root.join("Cargo.toml"),
            "[package]\nname=\"probe\"\nversion=\"0.1.0\"\n[lints]\nworkspace = true\n",
        )
        .unwrap();
        fs::write(
            crate_root.join("src/lib.rs"),
            "#[cfg(target_os = \"macos\")]\npub mod apple;\n",
        )
        .unwrap();
        fs::write(
            crate_root.join("src/apple/mod.rs"),
            "pub fn f() { println!(\"x\"); }\n",
        )
        .unwrap();

        let mut violations = Vec::new();
        scan_rust(root, &mut violations).unwrap();
        assert!(
            violations.is_empty(),
            "cfg(target_os=macos) mod subtree should be excluded on linux: {:?}",
            violations
        );
    }

    #[test]
    fn rust_out_of_line_mod_without_cfg_is_scanned() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let crate_root = root.join("sdk/streamlib-macros");
        fs::create_dir_all(crate_root.join("src")).unwrap();
        fs::write(
            crate_root.join("Cargo.toml"),
            "[package]\nname=\"probe\"\nversion=\"0.1.0\"\n[lints]\nworkspace = true\n",
        )
        .unwrap();
        fs::write(crate_root.join("src/lib.rs"), "pub mod submod;\n").unwrap();
        fs::write(
            crate_root.join("src/submod.rs"),
            "pub fn f() { println!(\"x\"); }\n",
        )
        .unwrap();

        let mut violations = Vec::new();
        scan_rust(root, &mut violations).unwrap();
        assert_eq!(
            violations.len(),
            1,
            "ungated submod should be linted: {:?}",
            violations
        );
    }

    #[test]
    fn rust_crate_without_workspace_lints_is_skipped() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let crate_root = root.join("runtime/streamlib-binary");
        fs::create_dir_all(crate_root.join("src")).unwrap();
        fs::write(
            crate_root.join("Cargo.toml"),
            "[package]\nname=\"binary\"\nversion=\"0.1.0\"\n", // no [lints]
        )
        .unwrap();
        fs::write(
            crate_root.join("src/main.rs"),
            "fn main() { println!(\"x\"); }\n",
        )
        .unwrap();

        let mut violations = Vec::new();
        scan_rust(root, &mut violations).unwrap();
        assert!(
            violations.is_empty(),
            "non-opt-in crate should be skipped entirely: {:?}",
            violations
        );
    }

    // ----- anti-vacuity ------------------------------------------------------

    /// One workspace-wide total cannot express this gate's contract: the
    /// polyglot scan and the Rust scan read independent trees, so the Rust half
    /// going to zero stays hidden behind the Python files that still scan. A
    /// tree holding only polyglot source must be refused.
    #[test]
    fn a_tree_whose_rust_source_roots_read_nothing_is_refused_despite_surviving_polyglot_files() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        write_fixture(
            &root.join(lint_target_named("python").root_relative),
            "streamlib/app.py",
            "streamlib.log.info(\"ok\")\n",
        );

        let report = scan_all(root).unwrap();
        assert!(report.violations.is_empty(), "{:?}", report.violations);
        assert!(
            report.files_scanned() > 0,
            "the polyglot half must read source, or this fixture proves nothing"
        );

        let error = run(root).expect_err(
            "a run whose Rust source roots read nothing must be refused, not reported clean",
        );
        assert!(
            error.to_string().contains("scanned 0 files"),
            "refusal must name the empty scan, got: {error}"
        );
    }

    /// The non-empty assertion alone does not prove each source root is
    /// reached: the Python SDK's files satisfy a workspace-wide total while
    /// every Rust crate goes unscanned. Pin every root against the real
    /// workspace so a renamed one cannot pass vacuously.
    #[test]
    fn the_logging_scan_reaches_every_polyglot_and_rust_source_root() {
        let workspace = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("xtask has no parent")
            .to_path_buf();
        let report = scan_all(&workspace).expect("scan");

        assert_eq!(
            report.scan_root_file_counts.len(),
            TARGETS.len() + crate::RUST_CRATE_SOURCE_ROOT_DIR_NAMES.len(),
            "every polyglot target and every Rust source root must report its own count"
        );
        for count in &report.scan_root_file_counts {
            assert!(
                count.files_scanned > 0,
                "no file was scanned under {} — the gate is passing vacuously there",
                count.scan_root_description
            );
        }
    }
}
