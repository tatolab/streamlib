// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Building an in-tree folder-backed package from an integration test.
//!
//! A package's `[lib] path` points at a generated crate root, and cargo resolves
//! that path at target resolution — before any build script runs — so it has to
//! exist before cargo is invoked. Off the monorepo the build orchestrator's
//! pre-cargo staging step writes it; in-tree there is no orchestrator in the
//! loop, so a test that shells out to `cargo build -p <pkg>` writes it first,
//! through the same generator (`cargo xtask generate-crate-roots` is the CI
//! entry point to it).

use std::path::Path;

use streamlib_processor_extract::crate_root::write_generated_rust_crate_roots_under;

/// Write every in-tree folder-backed package's crate root, then
/// `cargo build -p <cargo_package_name>`. Panics with the cargo failure, which
/// is what the caller wants: a fixture cdylib that did not build cannot be
/// dlopen'd, and a silent skip would turn a load test green for the wrong
/// reason.
pub fn build_folder_backed_package(cargo_package_name: &str) {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap();

    write_generated_rust_crate_roots_under(workspace_root).unwrap_or_else(|e| {
        panic!(
            "generating folder-backed crate roots under {}: {e} — \
             `cargo build -p {cargo_package_name}` would then fail at target \
             resolution with a missing `[lib] path`",
            workspace_root.display()
        )
    });

    let status = std::process::Command::new(env!("CARGO"))
        .args(["build", "-p", cargo_package_name])
        .status()
        .expect("invoking cargo build");
    assert!(
        status.success(),
        "cargo build -p {cargo_package_name} must succeed"
    );
}
