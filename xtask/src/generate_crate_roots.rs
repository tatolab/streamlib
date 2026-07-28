// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Write the generated Rust crate root for every in-tree folder-backed package.
//!
//! A folder-backed package commits no crate root: its `[lib] path` points at
//! `_generated_rust_crate_root_/lib.rs`, which is the mechanical projection of `processors/`.
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
use streamlib_processor_extract::crate_root::write_generated_rust_crate_roots_under;

/// Generate the crate root for every in-tree package whose `Cargo.toml` points
/// `[lib] path` at the generated location, and report what was written.
pub fn run(workspace_root: &Path) -> Result<()> {
    let written = write_generated_rust_crate_roots_under(workspace_root).with_context(|| {
        format!(
            "generating folder-backed crate roots under {}",
            workspace_root.display()
        )
    })?;

    for crate_root in &written {
        tracing::info!(
            crate_root = %crate_root.strip_prefix(workspace_root).unwrap_or(crate_root).display(),
            "generated"
        );
    }

    tracing::info!(packages = written.len(), "crate roots generated");
    Ok(())
}
