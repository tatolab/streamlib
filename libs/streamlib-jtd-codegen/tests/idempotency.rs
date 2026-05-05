// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Idempotency integration tests for the deterministic-ordering pass
//! (issue #684, exit criterion: "Regenerating any in-tree schema is a no-op
//! against a clean tree"; test bullet: "Idempotency test: regenerate twice
//! in a row, diff is empty").
//!
//! For each runtime (Rust, Python, TypeScript), runs the full project
//! codegen against the in-tree `libs/streamlib/streamlib.yaml` manifest
//! into two separate temp dirs and asserts the trees are byte-identical.
//! The sibling unit tests in `ordering.rs` lock the sort function in
//! isolation; this file locks the end-to-end pipeline.
//!
//! Skipped (with a clear stderr message) when `jtd-codegen` v0.4.1 is not
//! on PATH — there is no value in running the rest of the test framework
//! against a missing binary.

mod common;

use common::{diff_dirs, run_project_codegen, skip_unless_jtd_codegen_available};
use streamlib_jtd_codegen::RuntimeTarget;
use tempfile::TempDir;

fn assert_regen_twice_byte_identical(test_name: &str, runtime: RuntimeTarget) {
    if skip_unless_jtd_codegen_available(test_name) {
        return;
    }

    let first = TempDir::new().expect("temp dir 1");
    let second = TempDir::new().expect("temp dir 2");

    run_project_codegen(runtime, first.path());
    run_project_codegen(runtime, second.path());

    let diffs = diff_dirs(first.path(), second.path(), &[]);
    assert!(
        diffs.is_empty(),
        "{test_name}: regen-twice not byte-identical:\n{}",
        diffs.join("\n")
    );
}

#[test]
fn regen_twice_is_byte_identical_rust() {
    assert_regen_twice_byte_identical("regen_twice_is_byte_identical_rust", RuntimeTarget::Rust);
}

#[test]
fn regen_twice_is_byte_identical_python() {
    assert_regen_twice_byte_identical(
        "regen_twice_is_byte_identical_python",
        RuntimeTarget::Python,
    );
}

#[test]
fn regen_twice_is_byte_identical_typescript() {
    assert_regen_twice_byte_identical(
        "regen_twice_is_byte_identical_typescript",
        RuntimeTarget::Typescript,
    );
}
