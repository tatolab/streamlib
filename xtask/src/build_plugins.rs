// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `cargo xtask build-plugins` — dev-inner-loop staging of in-tree
//! workspace packages.
//!
//! Walks every `packages/<name>/streamlib.yaml` (Rust-impl + schemas-only
//! both), invokes `cargo build` against each Rust-impl package's cdylib,
//! and stages the output hermetically at
//! `<workspace>/target/streamlib-plugins/<org>__<name>/` so the
//! runtime's `ModuleResolverStrategy::WorkspaceStaged` resolver can
//! resolve packages by canonical name without depending on the
//! original source tree layout.

use std::path::Path;

use anyhow::{Context, Result};
use streamlib_cargo_build::{
    discover_package_dirs, has_rust_runtime_processors, host_dylib_extension, host_target_triple,
    read_cargo_package_name, read_minimal_project_config, run_cargo_build,
    stage_package_for_dev_load, CargoProfile,
};

/// Walk the workspace's `packages/` dir, build every Rust-impl
/// package's cdylib, and stage each (Rust-impl + schemas-only both)
/// at `<workspace>/target/streamlib-plugins/<org>__<name>/`.
///
/// `release` switches between `--release` (production-shaped) and
/// the default dev profile (faster inner loop — the recommended
/// dev-mode default).
///
/// `filter` restricts the set to a subset of canonical
/// `@<org>/<name>` ids; empty filter means "every package."
pub fn run(workspace_root: &Path, release: bool, filter: &[String]) -> Result<()> {
    let packages_root = workspace_root.join("packages");
    if !packages_root.is_dir() {
        anyhow::bail!(
            "Workspace `packages/` directory not found at {}",
            packages_root.display()
        );
    }

    let staged_root = workspace_root.join("target").join("streamlib-plugins");
    std::fs::create_dir_all(&staged_root).with_context(|| {
        format!("Failed to create staged root {}", staged_root.display())
    })?;

    let host_triple = host_target_triple();
    let dylib_ext = host_dylib_extension();
    let profile = if release {
        CargoProfile::Release
    } else {
        CargoProfile::Dev
    };

    let package_dirs = discover_package_dirs(&[&packages_root])?;
    if package_dirs.is_empty() {
        tracing::warn!(
            "No workspace packages found under {} — nothing to stage",
            packages_root.display()
        );
        return Ok(());
    }

    let mut staged_count = 0;
    let mut skipped_unparseable = 0;
    let mut skipped_no_package = 0;

    for package_dir in package_dirs {
        let Some(config) = read_minimal_project_config(&package_dir)? else {
            skipped_unparseable += 1;
            continue;
        };
        let Some(metadata) = config.package.as_ref() else {
            skipped_no_package += 1;
            continue;
        };
        let canonical_id = format!("@{}/{}", metadata.org.as_str(), metadata.name.as_str());

        if !filter.is_empty() && !filter.iter().any(|f| f == &canonical_id) {
            continue;
        }

        if has_rust_runtime_processors(&config) {
            let cargo_name = read_cargo_package_name(&package_dir).with_context(|| {
                format!(
                    "Package {} declares Rust runtime processors but its Cargo.toml is missing or malformed",
                    canonical_id
                )
            })?;
            let built = run_cargo_build(&package_dir, &cargo_name, dylib_ext, profile)
                .with_context(|| format!("cargo build failed for {}", canonical_id))?;
            let staged = stage_package_for_dev_load(
                &package_dir,
                &staged_root,
                Some(&built),
                host_triple,
            )?;
            tracing::info!("Staged {} → {}", canonical_id, staged.display());
        } else {
            let staged =
                stage_package_for_dev_load(&package_dir, &staged_root, None, host_triple)?;
            tracing::info!(
                "Staged {} (schemas-only) → {}",
                canonical_id,
                staged.display()
            );
        }
        staged_count += 1;
    }

    tracing::info!(
        "build-plugins: staged {} package(s) at {} ({} profile)",
        staged_count,
        staged_root.display(),
        profile.label()
    );
    if skipped_unparseable > 0 {
        tracing::warn!(
            "build-plugins: skipped {} dirs with unparseable streamlib.yaml",
            skipped_unparseable
        );
    }
    if skipped_no_package > 0 {
        tracing::warn!(
            "build-plugins: skipped {} dirs with no [package] section",
            skipped_no_package
        );
    }

    Ok(())
}

