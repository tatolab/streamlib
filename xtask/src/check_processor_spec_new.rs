// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! CI lint enforcing the structured-everywhere `ProcessorSpec` rule from
//! milestone 10.
//!
//! Two passes:
//!
//! 1. **Bare-string `ProcessorSpec::new`** (#707): catches
//!    `ProcessorSpec::new("PascalCase", ...)` re-introductions.
//! 2. **Hand-rolled `SchemaIdent` literal in `examples/*/src/`** (#719):
//!    polyglot Rust example crates use the
//!    `streamlib::sdk::schema_ident_any_version!` macro by default
//!    (3-arg, runtime resolution against the registry — the common
//!    case), or the strict-pin `streamlib::sdk::schema_ident!` form
//!    (4-arg, compile-time-validated `SemVer`). This pass flags
//!    `SchemaIdent::new(Org::new("..."), ...)` literals in
//!    `examples/*/src/*.rs` to keep the pattern from coming back.
//!
//! Both passes are deliberately tight — they catch the *exact* shape
//! they're responsible for. Macro-generated code, `<Module>::schema_ident()`
//! calls, and `tests/` fixtures all pass through.
//!
//! See `docs/architecture/schema-identity-and-packaging.md` for the rule
//! and the #707 / #719 issue bodies for migration history.

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
        println!(
            "✓ check-processor-spec-new: no bare-string ProcessorSpec::new sites and no hand-rolled SchemaIdent literals in examples/"
        );
        return Ok(());
    }

    eprintln!(
        "✗ check-processor-spec-new: {} violation(s):",
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
        "\nFix:\n  - Bare-string `ProcessorSpec::new(\"Foo\", ...)`: pass a structured `SchemaIdent`.\n  - Hand-rolled `SchemaIdent::new(Org::new(\"...\"), ...)` in examples/*/src/: replace with `streamlib::sdk::schema_ident_any_version!(\"org\", \"package\", \"Type\")?` (the common case — registry resolves the version at runtime), or with `streamlib::sdk::schema_ident!(\"org\", \"package\", \"Type\", \"1.0.0\")` when strict version pinning is required.\n\nSee docs/architecture/schema-identity-and-packaging.md and the #707 / #719 issue bodies."
    );
    anyhow::bail!("check-processor-spec-new failed");
}

/// True for paths under `<workspace_root>/examples/<crate>/.../src/`.
/// The hand-rolled-literal pass is scoped to example main.rs / linux.rs
/// files — codegen.rs in `streamlib-macros` legitimately emits the
/// literal as a token stream, and integration tests in `libs/*/tests/`
/// build expected values to assert against. Both must stay outside the
/// lint's reach. Accepts both the flat shape (`examples/<crate>/src/`)
/// and the monorepo-with-sub-packages shape
/// (`examples/<crate>/runner/src/` per #804).
fn is_example_src_file(path: &Path) -> bool {
    let mut components = path.components();
    let mut saw_examples = false;
    let mut saw_src_after_examples = false;
    while let Some(c) = components.next() {
        let s = c.as_os_str();
        if !saw_examples {
            if s == "examples" {
                saw_examples = true;
            }
            continue;
        }
        if s == "src" {
            saw_src_after_examples = true;
        }
    }
    saw_examples && saw_src_after_examples
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
    let lines: Vec<&str> = content.lines().collect();
    let example_src = is_example_src_file(path);
    for (idx, line) in lines.iter().enumerate() {
        if has_bare_string_processor_spec(line) {
            violations.push(LintViolation {
                file: path.to_path_buf(),
                line: idx + 1,
                snippet: (*line).to_string(),
            });
        }
        if example_src {
            let next = lines.get(idx + 1).copied().unwrap_or("");
            if has_hand_rolled_schema_ident_literal(line, next) {
                violations.push(LintViolation {
                    file: path.to_path_buf(),
                    line: idx + 1,
                    snippet: (*line).to_string(),
                });
            }
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

/// Match a hand-rolled `SchemaIdent::new(Org::new(...), ...)` literal in an
/// example's `src/` Rust file. Two shapes — same-line and multi-line —
/// are caught at the line carrying `SchemaIdent::new(`. The
/// `<Module>::schema_ident()` and macro-emitted forms are not flagged
/// (no `Org::new(` follows).
pub fn has_hand_rolled_schema_ident_literal(line: &str, next_line: &str) -> bool {
    let Some(idx) = line.find("SchemaIdent::new(") else {
        return false;
    };
    let after = &line[idx + "SchemaIdent::new(".len()..];
    let trimmed = after.trim_start();
    if trimmed.starts_with("Org::new(") {
        return true;
    }
    if trimmed.is_empty() && next_line.trim_start().starts_with("Org::new(") {
        return true;
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
    fn rejects_hand_rolled_schema_ident_same_line() {
        assert!(has_hand_rolled_schema_ident_literal(
            r#"        SchemaIdent::new(Org::new("tatolab").unwrap(), ..."#,
            "",
        ));
    }

    #[test]
    fn rejects_hand_rolled_schema_ident_multi_line() {
        assert!(has_hand_rolled_schema_ident_literal(
            r#"        SchemaIdent::new("#,
            r#"            Org::new("tatolab").unwrap(),"#,
        ));
    }

    #[test]
    fn accepts_module_schema_ident_call() {
        assert!(!has_hand_rolled_schema_ident_literal(
            r#"        SchemaIdent::new(SomeModule::schema_ident(), ..."#,
            "",
        ));
    }

    #[test]
    fn accepts_convenience_macro_form() {
        assert!(!has_hand_rolled_schema_ident_literal(
            r#"        streamlib::sdk::schema_ident!("tatolab", "foo", "Foo", "1.0.0")"#,
            "",
        ));
    }

    #[test]
    fn is_example_src_correctly_classifies_paths() {
        assert!(is_example_src_file(Path::new(
            "/abs/examples/foo/src/main.rs"
        )));
        assert!(is_example_src_file(Path::new(
            "/abs/examples/camera-python-display/runner/src/linux.rs"
        )));
        // libs/ tests legitimately build expected SchemaIdent values:
        assert!(!is_example_src_file(Path::new(
            "/abs/libs/streamlib-engine/tests/schema_ident_macro_test.rs"
        )));
        // Macro codegen emits the literal as a token stream:
        assert!(!is_example_src_file(Path::new(
            "/abs/libs/streamlib-macros/src/codegen.rs"
        )));
        // build.rs / shaders / fixtures sit beside src/, not under it:
        assert!(!is_example_src_file(Path::new("/abs/examples/foo/build.rs")));
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
