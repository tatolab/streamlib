// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Write the generated Rust crate root for every in-tree folder-backed package.
//!
//! A folder-backed package commits no crate root: its `[lib] path` points at
//! `_generated_/lib.rs`, which is the mechanical projection of `processors/`.
//! Off the monorepo, the build orchestrator's pre-cargo staging step writes it.
//! In-tree there is no orchestrator in the loop — `cargo test --workspace`, the
//! engine integration tests' `cargo build -p` shell-outs, and a bare
//! `cd packages/x && cargo build` all reach cargo directly — so this task is the
//! generation site for the monorepo, invoked by CI before `cargo test` and by
//! the integration tests before they shell out.
//!
//! Cargo resolves `[lib] path` at target resolution, before any build script
//! runs, so this cannot be a `build.rs` step.

use std::path::Path;

use anyhow::{Context, Result};
use streamlib_processor_extract::crate_root::{
    RustCrateRootGenerationRequest, discover_package_dirs_declaring_a_generated_crate_root,
    write_generated_rust_crate_root,
};

/// Generate the crate root for every in-tree package whose `Cargo.toml` points
/// `[lib] path` at the generated location, and report what was written.
pub fn run(workspace_root: &Path) -> Result<()> {
    let package_dirs = discover_package_dirs_declaring_a_generated_crate_root(workspace_root)
        .with_context(|| {
            format!(
                "discovering folder-backed packages under {}",
                workspace_root.display()
            )
        })?;
    anyhow::ensure!(
        !package_dirs.is_empty(),
        "no folder-backed package found under {} — a generation pass that \
         generates nothing would let every package's crate root go stale \
         unnoticed",
        workspace_root.display()
    );

    for package_dir in &package_dirs {
        let request = RustCrateRootGenerationRequest::for_package_dir(package_dir)
            .with_context(|| format!("reading {}", package_dir.display()))?;
        let written = write_generated_rust_crate_root(&request)
            .with_context(|| format!("generating crate root for {}", package_dir.display()))?;
        tracing::info!(
            crate_root = %written.strip_prefix(workspace_root).unwrap_or(&written).display(),
            "generated"
        );
    }

    tracing::info!(packages = package_dirs.len(), "crate roots generated");
    Ok(())
}
