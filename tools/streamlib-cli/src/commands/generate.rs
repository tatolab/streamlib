// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `streamlib generate` — codegen subcommand.
//!
//! Drives the JTD-codegen pipeline (extracted in #400, resolver-driven
//! since #402) so non-Rust developers can regenerate bindings without
//! installing rustup. Mirrors `cargo xtask generate-schemas` exactly —
//! same input modes, same output.

use std::path::PathBuf;

use anyhow::{Context, Result};
use streamlib_jtd_codegen::{GenerateOptions, RuntimeTarget, generate};

/// Run `streamlib generate`.
pub fn run(
    runtime: RuntimeTarget,
    output: PathBuf,
    project_dir: Option<PathBuf>,
    schema_file: Option<PathBuf>,
    schema_dir: Option<PathBuf>,
) -> Result<()> {
    // The user-facing `streamlib generate` resolves the active `streamlib link`
    // checkout MARKER-FIRST from the project dir (with STREAMLIB_LINK_CHECKOUT as
    // an override) — a dev running `streamlib generate` in their linked app dir
    // picks up the link with no env exported. (The build orchestrator's
    // in-process codegen supplies its own authoritative link state instead of
    // marker discovery; see `streamlib_jtd_codegen::generate`.)
    let link_checkout = project_dir
        .as_deref()
        .and_then(|dir| streamlib_idents::ResolverOptions::from_env_or_marker(dir).link_checkout);
    generate(GenerateOptions {
        runtime,
        output,
        project_dir,
        schema_file,
        schema_dir,
        workspace_root: workspace_root()?,
        write_lockfile: true,
        link_checkout,
    })
}

/// Resolve the workspace root the same way `cargo xtask` does — by asking
/// cargo. Mirrors `xtask::workspace_root` so the two entry points behave
/// identically when resolving project-relative paths.
fn workspace_root() -> Result<PathBuf> {
    let output = std::process::Command::new("cargo")
        .args(["locate-project", "--workspace", "--message-format=plain"])
        .output()
        .context("Failed to run cargo locate-project")?;

    let path = String::from_utf8(output.stdout)
        .context("Invalid UTF-8 in cargo output")?
        .trim()
        .to_string();

    PathBuf::from(path)
        .parent()
        .map(|p| p.to_path_buf())
        .context("Failed to get workspace root")
}
