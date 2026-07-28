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

use streamlib_processor_extract::crate_root::{
    RustCrateRootGenerationRequest, discover_package_dirs_declaring_a_generated_crate_root,
    write_generated_rust_crate_root,
};

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

    let package_dirs = discover_package_dirs_declaring_a_generated_crate_root(workspace_root)
        .expect("discovering folder-backed packages");
    assert!(
        !package_dirs.is_empty(),
        "no folder-backed package found under {} — generating nothing would let \
         `cargo build -p {cargo_package_name}` fail at target resolution with a \
         missing `[lib] path`",
        workspace_root.display()
    );
    for package_dir in package_dirs {
        let request = RustCrateRootGenerationRequest::for_package_dir(&package_dir)
            .unwrap_or_else(|e| panic!("reading {}: {e}", package_dir.display()));
        write_generated_rust_crate_root(&request)
            .unwrap_or_else(|e| panic!("generating crate root for {}: {e}", package_dir.display()));
    }

    let status = std::process::Command::new(env!("CARGO"))
        .args(["build", "-p", cargo_package_name])
        .status()
        .expect("invoking cargo build");
    assert!(
        status.success(),
        "cargo build -p {cargo_package_name} must succeed"
    );
}
