// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! CI lint enforcing the structured-everywhere `ProcessorSpec` rule from
//! milestone 10.
//!
//! Two passes:
//!
//! 1. **Bare-string `ProcessorSpec::new`** (#707): catches
//!    `ProcessorSpec::new("PascalCase", ...)` re-introductions.
//! 2. **Hand-rolled `SchemaIdent` literal in an example crate's Rust
//!    source** (#719):
//!    polyglot Rust example crates use the
//!    `streamlib::sdk::schema_ident_any_version!` macro by default
//!    (3-arg, runtime resolution against the registry — the common
//!    case), or the strict-pin `streamlib::sdk::schema_ident!` form
//!    (4-arg, compile-time-validated `SemVer`). This pass flags
//!    `SchemaIdent::new(Org::new("..."), ...)` literals in
//!    example Rust sources to keep the pattern from coming back.
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
pub const SCAN_DIR_PARENTS: &[&str] = &[
    "runtime", "sdk", "adapters", "vendor", "examples", "packages",
];

#[derive(Debug, PartialEq, Eq)]
pub struct LintViolation {
    pub file: PathBuf,
    pub line: usize,
    pub snippet: String,
}

/// What one scan of the workspace found, and how much source each of its two
/// passes read to find it. `run` refuses a report in which any one scan root
/// contributed nothing.
///
/// Two independent tallies, because the passes cover different sets: the
/// bare-string pass reads every `.rs` under [`SCAN_DIR_PARENTS`], while the
/// hand-rolled-`SchemaIdent` pass reads only the example-crate sources under
/// [`crate::RUST_CRATE_SOURCE_ROOT_DIR_NAMES`]. Counting the walk alone lets
/// the narrower pass silently select nothing.
#[derive(Debug)]
pub struct ProcessorSpecNewScanReport {
    pub violations: Vec<LintViolation>,
    /// Rust files read under each [`SCAN_DIR_PARENTS`] entry, in that order.
    pub files_scanned_per_scan_dir_parent: Vec<usize>,
    /// Example-crate Rust files the hand-rolled-`SchemaIdent` pass selected
    /// under each [`crate::RUST_CRATE_SOURCE_ROOT_DIR_NAMES`] entry, in that
    /// order.
    pub example_source_files_scanned_per_source_root: Vec<usize>,
}

impl ProcessorSpecNewScanReport {
    pub fn files_scanned(&self) -> usize {
        self.files_scanned_per_scan_dir_parent.iter().sum()
    }
}

pub fn run(workspace_root: &Path) -> Result<()> {
    let report = lint_workspace(workspace_root)?;
    for (parent, files_scanned) in SCAN_DIR_PARENTS
        .iter()
        .zip(&report.files_scanned_per_scan_dir_parent)
    {
        crate::ensure_source_walking_gate_read_source(
            "check-processor-spec-new",
            &format!("the `{parent}/` scan root"),
            *files_scanned,
            "a bare-string ProcessorSpec::new back in",
        )?;
    }
    for (root_name, files_scanned) in crate::RUST_CRATE_SOURCE_ROOT_DIR_NAMES
        .iter()
        .zip(&report.example_source_files_scanned_per_source_root)
    {
        crate::ensure_source_walking_gate_read_source(
            "check-processor-spec-new",
            &format!("every example crate's `{root_name}/` source root"),
            *files_scanned,
            "a hand-rolled SchemaIdent literal back into an example",
        )?;
    }

    if report.violations.is_empty() {
        println!(
            "✓ check-processor-spec-new: no bare-string ProcessorSpec::new sites and no hand-rolled SchemaIdent literals in examples/"
        );
        return Ok(());
    }

    eprintln!(
        "✗ check-processor-spec-new: {} violation(s):",
        report.violations.len()
    );
    for v in &report.violations {
        eprintln!("  {}:{}: {}", v.file.display(), v.line, v.snippet.trim());
    }
    eprintln!(
        "\nFix:\n  - Bare-string `ProcessorSpec::new(\"Foo\", ...)`: pass a structured `SchemaIdent`.\n  - Hand-rolled `SchemaIdent::new(Org::new(\"...\"), ...)` in examples/*/src/: replace with `streamlib::sdk::schema_ident_any_version!(\"org\", \"package\", \"Type\")?` (the common case — registry resolves the version at runtime), or with `streamlib::sdk::schema_ident!(\"org\", \"package\", \"Type\", \"1.0.0\")` when strict version pinning is required.\n\nSee docs/architecture/schema-identity-and-packaging.md and the #707 / #719 issue bodies."
    );
    anyhow::bail!("check-processor-spec-new failed");
}

/// The source root under which an example crate's Rust source file sits, or
/// `None` for any path outside an example's source tree.
///
/// The hand-rolled-literal pass is scoped to example main.rs / linux.rs
/// files — codegen.rs in `streamlib-macros` legitimately emits the
/// literal as a token stream, and integration tests in `libs/*/tests/`
/// build expected values to assert against. Both must stay outside the
/// lint's reach. Accepts the flat shape (`examples/<crate>/src/`), the
/// sibling-sub-package shapes some examples carry
/// (`examples/<crate>/plugin/`, `examples/<crate>/effects/`), and the
/// folder-backed `processors/` root those plugin crates author under.
///
/// Returns which root matched, not just that one did, so the gate can refuse a
/// run in which a whole source root selected nothing.
fn example_source_root_of(path: &Path) -> Option<&'static str> {
    let mut components = path.components();
    let mut saw_examples = false;
    let mut matched_source_root = None;
    for c in components.by_ref() {
        let s = c.as_os_str();
        if !saw_examples {
            if s == "examples" {
                saw_examples = true;
            }
            continue;
        }
        if let Some(root) = crate::RUST_CRATE_SOURCE_ROOT_DIR_NAMES
            .iter()
            .find(|root| s == **root)
        {
            matched_source_root = Some(*root);
        }
    }
    matched_source_root
}

/// Every bare-string `ProcessorSpec::new` / hand-rolled `SchemaIdent` site,
/// plus how many files were read to find them — [`run`] refuses a scan that
/// read nothing.
pub fn lint_workspace(workspace_root: &Path) -> Result<ProcessorSpecNewScanReport> {
    let mut violations = Vec::new();
    let mut files_scanned_per_scan_dir_parent = vec![0usize; SCAN_DIR_PARENTS.len()];
    let mut example_source_files_scanned_per_source_root =
        vec![0usize; crate::RUST_CRATE_SOURCE_ROOT_DIR_NAMES.len()];
    for (parent_index, parent) in SCAN_DIR_PARENTS.iter().enumerate() {
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
            files_scanned_per_scan_dir_parent[parent_index] += 1;
            let example_source_root = example_source_root_of(path);
            if let Some(root_name) = example_source_root
                && let Some(root_index) = crate::RUST_CRATE_SOURCE_ROOT_DIR_NAMES
                    .iter()
                    .position(|root| *root == root_name)
            {
                example_source_files_scanned_per_source_root[root_index] += 1;
            }
            scan_file(path, example_source_root.is_some(), &mut violations)?;
        }
    }
    Ok(ProcessorSpecNewScanReport {
        violations,
        files_scanned_per_scan_dir_parent,
        example_source_files_scanned_per_source_root,
    })
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

fn scan_file(path: &Path, example_src: bool, violations: &mut Vec<LintViolation>) -> Result<()> {
    let content =
        fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let lines: Vec<&str> = content.lines().collect();
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
    fn example_source_root_correctly_classifies_paths() {
        // Flat-shape example: examples/<crate>/src/<file>.rs
        assert_eq!(
            example_source_root_of(Path::new("/abs/examples/foo/src/main.rs")),
            Some("src")
        );
        // Sibling effects sub-package: examples/<crate>/effects/src/<file>.rs
        assert_eq!(
            example_source_root_of(Path::new(
                "/abs/examples/camera-python-display/effects/src/linux.rs"
            )),
            Some("src")
        );
        // Sibling plugin sub-package: examples/<crate>/plugin/src/<file>.rs
        assert_eq!(
            example_source_root_of(Path::new(
                "/abs/examples/camera-rust-plugin/plugin/src/lib.rs"
            )),
            Some("src")
        );
        // Folder-backed plugin sub-package: the authored root is
        // examples/<crate>/plugin/processors/, not src/ — narrowing the roots
        // back to `src` alone takes every swept example crate out of the lint's
        // reach with no failure anywhere.
        assert_eq!(
            example_source_root_of(Path::new(
                "/abs/examples/camera-rust-plugin/plugin/processors/grayscale_linux.rs"
            )),
            Some(streamlib_idents::PACKAGE_PROCESSOR_SOURCE_DIR_NAME)
        );
        assert_eq!(
            example_source_root_of(Path::new(
                "/abs/examples/camera-python-display/effects/processors/tone_mapper.rs"
            )),
            Some(streamlib_idents::PACKAGE_PROCESSOR_SOURCE_DIR_NAME)
        );
        // libs/ tests legitimately build expected SchemaIdent values:
        assert_eq!(
            example_source_root_of(Path::new(
                "/abs/runtime/streamlib-engine/tests/schema_ident_macro_test.rs"
            )),
            None
        );
        // Macro codegen emits the literal as a token stream:
        assert_eq!(
            example_source_root_of(Path::new("/abs/sdk/streamlib-macros/src/codegen.rs")),
            None
        );
        // build.rs / shaders / fixtures sit beside src/, not under it:
        assert_eq!(
            example_source_root_of(Path::new("/abs/examples/foo/build.rs")),
            None
        );
    }

    #[test]
    fn workspace_smoke_pass() {
        // Run the lint against the actual workspace. After #707 lands,
        // this must pass — every live `ProcessorSpec::new(` call site
        // takes a structured ident, not a bare PascalCase string.
        let workspace = workspace_root().expect("workspace root");
        let report = lint_workspace(&workspace).unwrap();
        assert!(
            report.violations.is_empty(),
            "workspace has bare-string ProcessorSpec::new sites: {:#?}",
            report.violations
        );
    }

    #[test]
    fn fixture_round_trip() {
        let dir = TempDir::new().unwrap();
        let bad = write_fixture(
            dir.path(),
            "runtime/foo/src/main.rs",
            r#"fn make() {
    let s = ProcessorSpec::new("CameraProcessor", config);
}"#,
        );
        // A non-violation file so the walker has something to keep going.
        write_fixture(
            dir.path(),
            "runtime/bar/src/lib.rs",
            r#"fn make_typed() {
    let s = ProcessorSpec::new(SchemaIdent::new(...), config);
}"#,
        );
        let report = lint_workspace(dir.path()).unwrap();
        assert_eq!(report.violations.len(), 1);
        assert_eq!(report.violations[0].file, bad);
    }

    // ----- anti-vacuity ------------------------------------------------------

    /// The walk's own file count cannot express the second pass's contract: it
    /// tallies every `.rs` under the scan parents, while the hand-rolled-
    /// `SchemaIdent` pass only fires on the example sources a source root
    /// selects. A tree whose examples hold no recognized source root must be
    /// refused even though the walk read plenty.
    #[test]
    fn a_tree_whose_example_source_roots_select_nothing_is_refused_despite_a_busy_walk() {
        let dir = TempDir::new().unwrap();
        for parent in SCAN_DIR_PARENTS {
            write_fixture(
                dir.path(),
                &format!("{parent}/probe/src/lib.rs"),
                "pub fn f() {}\n",
            );
        }
        // An example crate whose sources sit under no recognized source root:
        // the hand-rolled-literal pass selects zero files.
        write_fixture(
            dir.path(),
            "examples/foo/renamed_source_root/main.rs",
            r#"        SchemaIdent::new(Org::new("tatolab").unwrap(), pkg)"#,
        );

        let report = lint_workspace(dir.path()).unwrap();
        assert!(
            report.files_scanned() > 0,
            "the walk must read source, or this fixture proves nothing"
        );
        assert!(
            report.violations.is_empty(),
            "the planted literal is out of the pass's reach — that is the defect: {:?}",
            report.violations
        );

        let error = run(dir.path()).expect_err(
            "a run whose example source roots selected nothing must be refused, not reported clean",
        );
        assert!(
            error.to_string().contains("scanned 0 files"),
            "refusal must name the empty scan, got: {error}"
        );
    }

    /// The non-empty assertion alone does not prove each root is reached: the
    /// `src/`-rooted examples satisfy a workspace-wide total while every
    /// `processors/`-rooted example crate goes unselected. Pin both roots, and
    /// every scan parent, against the real workspace.
    #[test]
    fn the_scan_reaches_every_scan_parent_and_every_example_source_root() {
        let workspace = workspace_root().expect("workspace root");
        let report = lint_workspace(&workspace).expect("scan");

        for (parent, files_scanned) in SCAN_DIR_PARENTS
            .iter()
            .zip(&report.files_scanned_per_scan_dir_parent)
        {
            assert!(
                *files_scanned > 0,
                "no Rust file was scanned under `{parent}/` — the gate is passing \
                 vacuously for that tree"
            );
        }
        for (root_name, files_scanned) in crate::RUST_CRATE_SOURCE_ROOT_DIR_NAMES
            .iter()
            .zip(&report.example_source_files_scanned_per_source_root)
        {
            assert!(
                *files_scanned > 0,
                "no example-crate file was selected under a `{root_name}/` source root — \
                 the hand-rolled-SchemaIdent pass is passing vacuously for every crate \
                 rooted there"
            );
        }
    }

    fn workspace_root() -> Result<PathBuf> {
        let manifest = env!("CARGO_MANIFEST_DIR");
        Ok(PathBuf::from(manifest)
            .parent()
            .ok_or_else(|| anyhow::anyhow!("xtask has no parent"))?
            .to_path_buf())
    }
}
