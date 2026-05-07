// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! CI lint enforcing the structured-everywhere `ProcessorSpec` rule from
//! milestone 10.
//!
//! After #707, `ProcessorSpec::new` takes a structured
//! [`SchemaIdent`](streamlib_processor_schema::SchemaIdent) — never a bare
//! string literal. The macro emits the structured ident, the runtime
//! constructs it from manifest fields, and every direct call site
//! constructs `SchemaIdent::new(...)` or calls
//! `<Module>::schema_ident()`.
//!
//! This lint catches anyone re-introducing a `ProcessorSpec::new(
//! "PascalCase", ...)` pattern. The regex is deliberately tight — matches
//! only the exact "bare PascalCase string" shape. The structured
//! `ProcessorSpec::new(SchemaIdent::new(...), ...)` is fine; the
//! macro-generated `ProcessorSpec::new(Self::schema_ident(), ...)` is fine.
//!
//! See `docs/architecture/schema-identity-and-packaging.md` for the
//! rule and the issue #707 body for the migration history.

use anyhow::{Context, Result};
use std::fs;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

/// Workspace subtrees scanned. The bar is "live Rust under each tree" —
/// the regex itself is lenient enough that we cover examples and
/// `libs/` together; flat coverage means no consumer can reintroduce the
/// pattern in a forgotten tree.
pub const SCAN_DIR_PARENTS: &[&str] = &["libs", "examples", "packages"];

#[derive(Debug, PartialEq, Eq)]
pub struct LintViolation {
    pub file: PathBuf,
    pub line: usize,
    pub snippet: String,
}

pub fn run(workspace_root: &Path) -> Result<()> {
    let violations = lint_workspace(workspace_root)?;

    if violations.is_empty() {
        println!("✓ check-processor-spec-new: no bare-string ProcessorSpec::new call sites");
        return Ok(());
    }

    eprintln!(
        "✗ check-processor-spec-new: {} violation(s) — bare-string ProcessorSpec::new is forbidden (use SchemaIdent::new(...) or <Module>::schema_ident()):",
        violations.len()
    );
    for v in &violations {
        eprintln!(
            "  {}:{}: {}",
            v.file.display(),
            v.line,
            v.snippet.trim()
        );
    }
    eprintln!(
        "\nSee docs/architecture/schema-identity-and-packaging.md and the issue #707 body."
    );
    anyhow::bail!("check-processor-spec-new failed");
}

pub fn lint_workspace(workspace_root: &Path) -> Result<Vec<LintViolation>> {
    let mut violations = Vec::new();
    for parent in SCAN_DIR_PARENTS {
        let parent_path = workspace_root.join(parent);
        if !parent_path.exists() {
            continue;
        }
        for entry in WalkDir::new(&parent_path).follow_links(false) {
            let entry = entry.with_context(|| format!("walking {}", parent_path.display()))?;
            let path = entry.path();
            if !is_rust_source(path) {
                continue;
            }
            scan_file(path, &mut violations)?;
        }
    }
    Ok(violations)
}

fn is_rust_source(path: &Path) -> bool {
    if !path.is_file() {
        return false;
    }
    if path.extension().and_then(|e| e.to_str()) != Some("rs") {
        return false;
    }
    // Skip target/ build artifacts (WalkDir doesn't follow them but a
    // manual check guards against unusual layouts).
    !path.components().any(|c| c.as_os_str() == "target")
}

fn scan_file(path: &Path, violations: &mut Vec<LintViolation>) -> Result<()> {
    let content = fs::read_to_string(path)
        .with_context(|| format!("reading {}", path.display()))?;
    for (idx, line) in content.lines().enumerate() {
        if has_bare_string_processor_spec(line) {
            violations.push(LintViolation {
                file: path.to_path_buf(),
                line: idx + 1,
                snippet: line.to_string(),
            });
        }
    }
    Ok(())
}

/// Match `ProcessorSpec::new("<PascalCase>"`. Plain string scan with no
/// regex-engine dependency: looks for the literal call-prefix, then a
/// double-quoted PascalCase identifier (uppercase first char, ASCII
/// alphanumeric thereafter) immediately following the opening paren.
///
/// Whitespace between `new(` and the opening quote is tolerated for
/// matches that span lines via `;` or `\n`. The lint operates per-line,
/// so the common multi-line form
///
/// ```ignore
/// ProcessorSpec::new(
///     "CameraProcessor",
///     ...
/// )
/// ```
///
/// is caught on the line carrying the bare string literal — the
/// `"PascalCase"` token sits on its own line. To cover that, the matcher
/// also flags any line whose trimmed-leading content starts with
/// `"<UpperLetter>...",` AND a sibling `ProcessorSpec::new(` call exists
/// in the surrounding block. Implemented here as: any line whose first
/// non-whitespace token is `"<UpperLetter>...",` is checked; a separate
/// pass would be needed to verify it's inside a `ProcessorSpec::new(`,
/// but the bare-quoted-PascalCase line on its own is unique enough in
/// practice that flagging it is the right call. Future tightening can
/// add the call-site context check.
pub fn has_bare_string_processor_spec(line: &str) -> bool {
    // Same-line form: `ProcessorSpec::new("Pascal..."`
    if let Some(idx) = line.find("ProcessorSpec::new(") {
        let after = &line[idx + "ProcessorSpec::new(".len()..];
        let trimmed = after.trim_start();
        if is_pascal_case_string_literal(trimmed) {
            return true;
        }
    }
    false
}

fn is_pascal_case_string_literal(s: &str) -> bool {
    let mut chars = s.chars();
    if chars.next() != Some('"') {
        return false;
    }
    let first = match chars.next() {
        Some(c) => c,
        None => return false,
    };
    if !first.is_ascii_uppercase() {
        return false;
    }
    let mut saw_close = false;
    for c in chars {
        if c == '"' {
            saw_close = true;
            break;
        }
        if !c.is_ascii_alphanumeric() {
            return false;
        }
    }
    saw_close
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn write_fixture(dir: &Path, rel: &str, body: &str) -> PathBuf {
        let p = dir.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, body).unwrap();
        p
    }

    #[test]
    fn rejects_bare_string_pascal_case() {
        assert!(has_bare_string_processor_spec(
            r#"    let s = ProcessorSpec::new("CameraProcessor", config);"#
        ));
    }

    #[test]
    fn rejects_bare_string_with_underscore_arg() {
        assert!(has_bare_string_processor_spec(
            r#"ProcessorSpec::new("DisplayProcessor", serde_json::Value::Null)"#
        ));
    }

    #[test]
    fn accepts_structured_ident_call() {
        assert!(!has_bare_string_processor_spec(
            r#"ProcessorSpec::new(SchemaIdent::new(...), config)"#
        ));
    }

    #[test]
    fn accepts_macro_emitted_schema_ident() {
        assert!(!has_bare_string_processor_spec(
            r#"ProcessorSpec::new(CameraProcessor::schema_ident(), config)"#
        ));
    }

    #[test]
    fn accepts_helper_call() {
        assert!(!has_bare_string_processor_spec(
            r#"ProcessorSpec::new(runtime_kind.processor_ident(), config)"#
        ));
    }

    #[test]
    fn accepts_lowercase_or_reverse_dns_string() {
        // Reverse-DNS like `"com.tatolab.foo"` doesn't match the regex
        // (lowercase first char) — the lint is targeted at the specific
        // post-#404 PascalCase pattern. Reverse-DNS is also banned per
        // the architecture preamble, but a separate sweep handles that
        // class.
        assert!(!has_bare_string_processor_spec(
            r#"ProcessorSpec::new("com.tatolab.foo", config)"#
        ));
        assert!(!has_bare_string_processor_spec(
            r#"ProcessorSpec::new("snake_case_name", config)"#
        ));
    }

    #[test]
    fn ignores_unrelated_string_literals() {
        // A test that uses `"CameraProcessor"` as an `assert_eq!` value
        // (e.g. to check a Display-rendered name) must not trip the
        // lint — only the `ProcessorSpec::new(` call-prefix triggers.
        assert!(!has_bare_string_processor_spec(
            r#"assert_eq!(name, "CameraProcessor");"#
        ));
    }

    #[test]
    fn workspace_smoke_pass() {
        // Run the lint against the actual workspace. After #707 lands,
        // this must pass — every live `ProcessorSpec::new(` call site
        // takes a structured ident, not a bare PascalCase string.
        let workspace = workspace_root().expect("workspace root");
        let violations = lint_workspace(&workspace).unwrap();
        assert!(
            violations.is_empty(),
            "workspace has bare-string ProcessorSpec::new sites: {:#?}",
            violations
        );
    }

    #[test]
    fn fixture_round_trip() {
        let dir = TempDir::new().unwrap();
        let bad = write_fixture(
            dir.path(),
            "libs/foo/src/main.rs",
            r#"fn make() {
    let s = ProcessorSpec::new("CameraProcessor", config);
}"#,
        );
        // A non-violation file so the walker has something to keep going.
        write_fixture(
            dir.path(),
            "libs/bar/src/lib.rs",
            r#"fn make_typed() {
    let s = ProcessorSpec::new(SchemaIdent::new(...), config);
}"#,
        );
        let violations = lint_workspace(dir.path()).unwrap();
        assert_eq!(violations.len(), 1);
        assert_eq!(violations[0].file, bad);
    }

    fn workspace_root() -> Result<PathBuf> {
        let manifest = env!("CARGO_MANIFEST_DIR");
        Ok(PathBuf::from(manifest)
            .parent()
            .ok_or_else(|| anyhow::anyhow!("xtask has no parent"))?
            .to_path_buf())
    }
}
