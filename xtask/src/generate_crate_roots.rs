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

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use streamlib_processor_extract::crate_root::{
    RustCrateRootGenerationRequest, write_generated_rust_crate_root,
};
use walkdir::WalkDir;

/// Generate the crate root for every in-tree package whose `Cargo.toml` points
/// `[lib] path` at the generated location, and report what was written.
pub fn run(workspace_root: &Path) -> Result<()> {
    let package_dirs = discover_folder_backed_package_dirs(workspace_root)?;
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

/// Every directory under the workspace whose `Cargo.toml` points `[lib] path`
/// at the generated crate root. Keying on the declared path — rather than on
/// "has a `processors/` directory" — means a package that opts in but whose
/// `processors/` went missing fails loudly at generation instead of silently
/// producing an empty crate.
pub fn discover_folder_backed_package_dirs(workspace_root: &Path) -> Result<Vec<PathBuf>> {
    let generated_lib_path = format!(
        "{}/{}",
        streamlib_processor_extract::crate_root::GENERATED_CRATE_ROOT_DIR_NAME,
        streamlib_processor_extract::crate_root::GENERATED_CRATE_ROOT_FILE_NAME
    );

    let mut out = Vec::new();
    let walker = WalkDir::new(workspace_root).into_iter().filter_entry(|e| {
        let name = e.file_name().to_string_lossy();
        if e.file_type().is_dir() {
            name != "target" && name != "node_modules" && !name.starts_with('.')
        } else {
            true
        }
    });
    for entry in walker.filter_map(|e| e.ok()) {
        if entry.file_name() != "Cargo.toml" {
            continue;
        }
        let manifest_path = entry.path();
        let body = std::fs::read_to_string(manifest_path)
            .with_context(|| format!("reading {}", manifest_path.display()))?;
        let Ok(manifest) = body.parse::<toml::Value>() else {
            continue;
        };
        let declares_generated_root = manifest
            .get("lib")
            .and_then(|lib| lib.get("path"))
            .and_then(|path| path.as_str())
            .is_some_and(|path| path == generated_lib_path);
        if declares_generated_root && let Some(dir) = manifest_path.parent() {
            out.push(dir.to_path_buf());
        }
    }
    out.sort();
    Ok(out)
}
