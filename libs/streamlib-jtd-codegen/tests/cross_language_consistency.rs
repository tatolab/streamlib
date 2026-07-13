// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Cross-language consistency integration test for the deterministic-
//! ordering pass (issue #684, test bullet: "Cross-language consistency
//! test: same schema's fields appear in the same order across the three
//! generated bindings").
//!
//! Runs jtd-codegen for one representative schema across Rust, Python,
//! and TypeScript, then parses the field-name order out of each emit and
//! asserts all three match the JTD properties' lexicographic order.
//!
//! The representative schema is `tests/fixtures/jtd_codegen_test_fixture.yaml`
//! — a test-only fixture with eight `optionalProperties` declared in
//! non-alphabetical order. A regression that bypasses the ordering pass
//! shows up as a mismatch. The fixture lives inside this crate (not in
//! a domain package) so the test doesn't break when production schemas
//! migrate into their own carve-out crates (#673, #674, #675, …).
//!
//! Skipped (with a clear stderr message) when `jtd-codegen` is not on PATH.

mod common;

use common::{run_single_schema_codegen, skip_unless_jtd_codegen_available, workspace_root};
use streamlib_jtd_codegen::RuntimeTarget;
use tempfile::TempDir;

const REPRESENTATIVE_SCHEMA_REL: &str =
    "libs/streamlib-jtd-codegen/tests/fixtures/jtd_codegen_test_fixture.yaml";

const EXPECTED_FIELDS: &[&str] = &[
    "alpha", "beta", "delta", "epsilon", "gamma", "mu", "nu", "zeta",
];

#[test]
fn field_order_consistent_across_runtimes_for_jtd_codegen_test_fixture() {
    let test_name = "field_order_consistent_across_runtimes_for_jtd_codegen_test_fixture";
    if skip_unless_jtd_codegen_available(test_name) {
        return;
    }

    let schema_path = workspace_root().join(REPRESENTATIVE_SCHEMA_REL);
    assert!(
        schema_path.exists(),
        "{test_name}: representative schema missing at {}",
        schema_path.display()
    );

    let rust_dir = TempDir::new().expect("rust temp dir");
    let py_dir = TempDir::new().expect("python temp dir");
    let ts_dir = TempDir::new().expect("typescript temp dir");

    run_single_schema_codegen(RuntimeTarget::Rust, &schema_path, rust_dir.path());
    run_single_schema_codegen(RuntimeTarget::Python, &schema_path, py_dir.path());
    run_single_schema_codegen(RuntimeTarget::Typescript, &schema_path, ts_dir.path());

    let rust_code = std::fs::read_to_string(rust_dir.path().join("jtd_codegen_test_fixture.rs"))
        .expect("read generated Rust");
    let py_code = std::fs::read_to_string(py_dir.path().join("jtd_codegen_test_fixture.py"))
        .expect("read generated Python");
    let ts_code = std::fs::read_to_string(ts_dir.path().join("jtd_codegen_test_fixture.ts"))
        .expect("read generated TypeScript");

    let rust_fields = extract_rust_struct_fields(&rust_code);
    let py_fields = extract_python_dataclass_fields(&py_code);
    let ts_fields = extract_typescript_interface_fields(&ts_code);

    // Catch a parser regression separately from an ordering regression: an
    // empty list means the line-based extractor stopped matching the emit
    // shape, not that the codegen produced an empty struct.
    assert!(
        !rust_fields.is_empty(),
        "rust field-name parser found no fields — emit shape may have changed"
    );
    assert!(
        !py_fields.is_empty(),
        "python field-name parser found no fields — emit shape may have changed"
    );
    assert!(
        !ts_fields.is_empty(),
        "typescript field-name parser found no fields — emit shape may have changed"
    );

    assert_eq!(
        rust_fields, EXPECTED_FIELDS,
        "rust field order differs from JTD lexicographic order"
    );
    assert_eq!(
        py_fields, EXPECTED_FIELDS,
        "python field order differs from JTD lexicographic order"
    );
    assert_eq!(
        ts_fields, EXPECTED_FIELDS,
        "typescript field order differs from JTD lexicographic order"
    );
}

/// Pull `pub <name>:` lines out of a generated Rust struct body.
fn extract_rust_struct_fields(code: &str) -> Vec<String> {
    let mut fields = Vec::new();
    let mut in_struct = false;
    for line in code.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("pub struct ") && trimmed.trim_end().ends_with('{') {
            in_struct = true;
            continue;
        }
        if !in_struct {
            continue;
        }
        if line.starts_with('}') {
            break;
        }
        if let Some(rest) = trimmed.strip_prefix("pub ")
            && let Some(colon) = rest.find(':')
        {
            let name = &rest[..colon];
            if is_ident(name) {
                fields.push(name.to_string());
            }
        }
    }
    fields
}

/// Pull `<name>: '<type>'` lines out of a generated Python `@dataclass`
/// body. Skips triple-quoted docstrings and stops at the first method.
fn extract_python_dataclass_fields(code: &str) -> Vec<String> {
    let mut fields = Vec::new();
    let mut in_class = false;
    let mut in_docstring = false;
    for line in code.lines() {
        if line.starts_with("class ") && line.trim_end().ends_with(':') {
            in_class = true;
            continue;
        }
        if !in_class {
            continue;
        }
        if line.trim() == "\"\"\"" {
            in_docstring = !in_docstring;
            continue;
        }
        if in_docstring {
            continue;
        }
        let trimmed = line.trim_start();
        if trimmed.starts_with("@classmethod") || trimmed.starts_with("def ") {
            break;
        }
        // Field lines have exactly four leading spaces.
        if let Some(after_indent) = line.strip_prefix("    ") {
            if after_indent.starts_with(' ') {
                continue;
            }
            if let Some(colon) = after_indent.find(':') {
                let name = &after_indent[..colon];
                if is_ident(name) {
                    fields.push(name.to_string());
                }
            }
        }
    }
    fields
}

/// Pull `<name>?:` / `<name>:` lines out of a generated TypeScript
/// `export interface` body. Skips `/** ... */` JSDoc comments.
fn extract_typescript_interface_fields(code: &str) -> Vec<String> {
    let mut fields = Vec::new();
    let mut in_interface = false;
    let mut in_jsdoc = false;
    for line in code.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("export interface ") {
            in_interface = true;
            continue;
        }
        if !in_interface {
            continue;
        }
        if line.starts_with('}') {
            break;
        }
        if trimmed.starts_with("/**") {
            in_jsdoc = true;
        }
        if in_jsdoc {
            if trimmed.ends_with("*/") {
                in_jsdoc = false;
            }
            continue;
        }
        if let Some(colon) = trimmed.find(':') {
            let name_part = trimmed[..colon].trim_end_matches('?').trim();
            if is_ident(name_part) {
                fields.push(name_part.to_string());
            }
        }
    }
    fields
}

fn is_ident(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}
