// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `streamlib generate` — codegen subcommand.
//!
//! Drives the JTD-codegen pipeline (extracted in #400) so non-Rust developers
//! can regenerate bindings without installing rustup. Mirrors
//! `cargo xtask generate-schemas` exactly — same input modes, same output.

use std::path::PathBuf;

use anyhow::{Context, Result};
use streamlib_jtd_codegen::{generate, GenerateOptions, RuntimeTarget};

/// Run `streamlib generate`.
pub fn run(
    runtime: RuntimeTarget,
    output: PathBuf,
    project_file: Option<PathBuf>,
    schema_file: Option<PathBuf>,
    schema_dir: Option<PathBuf>,
) -> Result<()> {
    generate(GenerateOptions {
        runtime,
        output,
        project_file,
        schema_file,
        schema_dir,
        workspace_root: workspace_root()?,
    })
}

/// Resolve the workspace root the same way `cargo xtask` does — by asking
/// cargo. Mirrors `xtask::workspace_root` so the two entry points behave
/// identically when resolving project-file-relative schema paths.
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
