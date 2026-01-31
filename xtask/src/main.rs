// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Build tasks for StreamLib development.
//!
//! Usage:
//!   cargo xtask generate-schemas --runtime rust --project-file libs/streamlib/Cargo.toml --output libs/streamlib/src/_generated_

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
    Typescript,
}

#[derive(Subcommand)]
enum Commands {
    /// Generate code from JTD schemas using jtd-codegen
    GenerateSchemas {
        /// Target language (default: rust)
        #[arg(long, default_value = "rust")]
        runtime: RuntimeTarget,

        /// Output directory (required)
        #[arg(long)]
        output: PathBuf,

        /// Read schema list from a project file (Cargo.toml or pyproject.toml)
        #[arg(long, group = "input")]
        project_file: Option<PathBuf>,

        /// Process a single schema file
        #[arg(long, group = "input")]
        schema_file: Option<PathBuf>,

        /// Process all .yaml files in a directory
        #[arg(long, group = "input")]
        schema_dir: Option<PathBuf>,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::GenerateSchemas {
            runtime,
            output,
            project_file,
            schema_file,
            schema_dir,
        } => generate_schemas::run(runtime, output, project_file, schema_file, schema_dir)?,
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
