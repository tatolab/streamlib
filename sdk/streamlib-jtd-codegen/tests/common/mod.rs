// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Shared helpers for the integration tests under `tests/`.
//!
//! Cargo treats `tests/common/mod.rs` as a non-test module — files at
//! `tests/<name>.rs` declare `mod common;` to pull these helpers in
//! without spawning a separate test binary for the helpers themselves.

#![allow(dead_code)] // each integration-test file pulls in only the helpers it uses

use std::path::{Path, PathBuf};
use std::process::Command;

use streamlib_jtd_codegen::{GenerateOptions, RuntimeTarget, generate};

/// True when `jtd-codegen --version` succeeds. The codegen pipeline shells
/// out to the binary, so tests that exercise the real pipeline are no-ops
/// without it.
pub fn jtd_codegen_available() -> bool {
    Command::new("jtd-codegen")
        .arg("--version")
        .output()
        .map(|out| out.status.success())
        .unwrap_or(false)
}

/// Returns `true` and prints a stderr skip message when `jtd-codegen` is
/// missing; otherwise returns `false`. Tests should `return` early on a
/// `true` result.
///
/// `eprintln!` here is the test-skip escape hatch that
/// `docs/logging.md` explicitly carves out — the goal is to surface the
/// skip reason to the developer reading `cargo test` output, regardless
/// of whether a tracing subscriber is configured.
#[allow(clippy::disallowed_macros)]
pub fn skip_unless_jtd_codegen_available(test_name: &str) -> bool {
    if jtd_codegen_available() {
        return false;
    }
    eprintln!("{test_name}: jtd-codegen not on PATH — skipping");
    true
}

/// Workspace root resolved from the test crate's `CARGO_MANIFEST_DIR`.
pub fn workspace_root() -> PathBuf {
    let manifest_dir =
        std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR is set by cargo");
    PathBuf::from(manifest_dir)
        .join("..")
        .join("..")
        .canonicalize()
        .expect("workspace root canonicalize")
}

/// `runtime/streamlib-engine` — directory holding the in-tree
/// `streamlib.yaml` manifest and `schemas/` directory.
pub fn streamlib_yaml_dir() -> PathBuf {
    workspace_root().join("runtime/streamlib-engine")
}

/// Run the full project codegen against the in-tree `streamlib.yaml`.
pub fn run_project_codegen(runtime: RuntimeTarget, output: &Path) {
    generate(GenerateOptions {
        runtime,
        output: output.to_path_buf(),
        project_dir: Some(streamlib_yaml_dir()),
        schema_file: None,
        schema_dir: None,
        workspace_root: workspace_root(),
        write_lockfile: false,
        link_checkout: None,
    })
    .expect("generate project codegen");
}

/// Run codegen for a single schema file (used by the cross-language test
/// to keep the loop tight against one representative schema).
pub fn run_single_schema_codegen(runtime: RuntimeTarget, schema_file: &Path, output: &Path) {
    generate(GenerateOptions {
        runtime,
        output: output.to_path_buf(),
        project_dir: None,
        schema_file: Some(schema_file.to_path_buf()),
        schema_dir: None,
        workspace_root: workspace_root(),
        write_lockfile: false,
        link_checkout: None,
    })
    .expect("generate single-schema codegen");
}

/// Recursively collect every file in `dir`, returning paths relative to
/// `dir`. Skips any directory or file whose basename appears in
/// `exclude_basenames` (e.g. Python's `__pycache__`).
pub fn collect_files(dir: &Path, exclude_basenames: &[&str]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    walk(dir, dir, &mut out, exclude_basenames);
    out.sort();
    out
}

fn walk(root: &Path, dir: &Path, out: &mut Vec<PathBuf>, exclude_basenames: &[&str]) {
    for entry in std::fs::read_dir(dir).expect("read_dir") {
        let entry = entry.expect("dir entry");
        let path = entry.path();
        let basename = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default();
        if exclude_basenames.contains(&basename) {
            continue;
        }
        if path.is_dir() {
            walk(root, &path, out, exclude_basenames);
        } else {
            out.push(path.strip_prefix(root).expect("strip_prefix").to_path_buf());
        }
    }
}

/// Compare two directories and return a list of human-readable
/// difference descriptions. Empty vec means the trees are byte-identical.
pub fn diff_dirs(a: &Path, b: &Path, exclude_basenames: &[&str]) -> Vec<String> {
    let a_files = collect_files(a, exclude_basenames);
    let b_files = collect_files(b, exclude_basenames);

    let mut differences = Vec::new();

    if a_files != b_files {
        let only_in_a: Vec<_> = a_files.iter().filter(|p| !b_files.contains(p)).collect();
        let only_in_b: Vec<_> = b_files.iter().filter(|p| !a_files.contains(p)).collect();
        if !only_in_a.is_empty() {
            differences.push(format!("only in {}: {:?}", a.display(), only_in_a));
        }
        if !only_in_b.is_empty() {
            differences.push(format!("only in {}: {:?}", b.display(), only_in_b));
        }
    }

    for rel in &a_files {
        if !b_files.contains(rel) {
            continue;
        }
        let av = std::fs::read(a.join(rel)).expect("read a");
        let bv = std::fs::read(b.join(rel)).expect("read b");
        if av != bv {
            differences.push(format!("{} differs", rel.display()));
        }
    }

    differences
}
