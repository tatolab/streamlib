// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Build tasks for StreamLib development.
//!
//! Usage:
//!   cargo xtask generate-schemas    Generate Rust structs from JTD schemas

use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use std::path::PathBuf;

mod generate_schemas;

#[derive(Parser)]
#[command(name = "xtask")]
#[command(about = "StreamLib development tasks")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

/// Target runtime language for schema code generation.
#[derive(Debug, Clone, ValueEnum)]
pub enum RuntimeTarget {
    Rust,
    Python,
}

#[derive(Subcommand)]
enum Commands {
    /// Generate code from JTD schemas defined in Cargo.toml or pyproject.toml
    GenerateSchemas {
        /// Target language (default: rust)
        #[arg(long, default_value = "rust")]
        runtime: RuntimeTarget,

        /// Output directory (default: libs/streamlib/src/_generated_/ for rust)
        #[arg(long)]
        output: Option<PathBuf>,

        /// Source file for schema list (default: libs/streamlib/Cargo.toml).
        /// Supports Cargo.toml ([package.metadata.streamlib].schemas)
        /// and pyproject.toml ([tool.streamlib].schemas).
        #[arg(long)]
        source: Option<PathBuf>,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::GenerateSchemas {
            runtime,
            output,
            source,
        } => generate_schemas::run(runtime, output, source)?,
    }

    Ok(())
}

/// Get the workspace root directory.
pub fn workspace_root() -> Result<PathBuf> {
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
