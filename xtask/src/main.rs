// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Build tasks for StreamLib development.
//!
//! For routine codegen, each Rust crate's `build.rs` invokes
//! `streamlib_jtd_codegen::build_rs::run_for_rust_crate` automatically.
//! This subcommand exists for ad-hoc generation and the Python / Deno
//! triggers (`setup.py` + `deno task setup`) that shell out to the CLI.

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::path::PathBuf;
use streamlib_jtd_codegen::{generate, GenerateOptions, RuntimeTarget};

pub mod build_plugins;
pub mod check_boundaries;
pub mod check_cdylib_reach;
pub mod check_no_reverse_dns;
pub mod check_no_streamlib_metadata;
pub mod check_processor_spec_new;
pub mod check_schema_versions;
pub mod lint_logging;
pub mod manifest_schema;

#[derive(Parser)]
#[command(name = "xtask")]
#[command(about = "StreamLib development tasks")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Generate code from JTD schemas using jtd-codegen.
    ///
    /// Thin wrapper around `streamlib-jtd-codegen`. The same pipeline is also
    /// reachable as `streamlib generate` for non-Rust developers (no rustup
    /// required).
    GenerateSchemas {
        /// Target language (default: rust)
        #[arg(long, default_value = "rust")]
        runtime: RuntimeTarget,

        /// Output directory (required)
        #[arg(long)]
        output: PathBuf,

        /// `streamlib.yaml`-driven mode: directory containing the manifest.
        /// The resolver walks declared dependencies and codegen ingests the
        /// resulting set.
        #[arg(long, group = "input")]
        project_dir: Option<PathBuf>,

        /// Process a single schema file
        #[arg(long, group = "input")]
        schema_file: Option<PathBuf>,

        /// Process all .yaml files in a directory
        #[arg(long, group = "input")]
        schema_dir: Option<PathBuf>,
    },

    /// Ban ad-hoc logging in polyglot SDK library code (Python + TypeScript).
    /// Paired with the workspace clippy.toml `disallowed-macros` rule for Rust.
    LintLogging,

    /// Boundary-grep CI gate for the Vulkan RHI capability split. Fails on
    /// `ash`, raw `vulkanalia` outside RHI/adapter crates, cdylibs depending
    /// on the full `streamlib` crate, or privileged Vulkan calls outside
    /// the RHI. See `docs/architecture/subprocess-rhi-parity.md`.
    CheckBoundaries,

    /// CI gate for the package-as-publication-unit rule from milestone 10.
    /// Fails when any schema YAML declares a top-level `version` key
    /// (versioning lives in `streamlib.yaml`, not in individual schemas).
    /// See `docs/architecture/schema-identity-and-packaging.md`.
    CheckSchemaVersions,

    /// CI gate for #402's atomic cutover off language-native metadata.
    /// Fails on `[package.metadata.streamlib]`, `[tool.streamlib]`, or a
    /// top-level `streamlib` key in `deno.json` / `deno.jsonc`. The single
    /// source of truth is `streamlib.yaml`; see
    /// `docs/architecture/schema-identity-and-packaging.md` (anti-pattern 4).
    CheckNoStreamlibMetadata,

    /// CI gate for milestone-10's structured-identifier rule. Fails on
    /// legacy reverse-DNS schema literals (`com.tatolab.*`,
    /// `com.streamlib.*`) anywhere in live workspace code. Apple
    /// platform code (`*/apple/*`), test code (`#[cfg(test)]`,
    /// `tests/`, `*_test{s}.rs`), and Rust comments are allowed. See
    /// `docs/architecture/schema-identity-and-packaging.md`.
    CheckNoReverseDns,

    /// CI gate for the structured-everywhere `ProcessorSpec` rule from
    /// #707. Fails on `ProcessorSpec::new("PascalCase", ...)` — every
    /// call site must take a structured `SchemaIdent` (built via
    /// `SchemaIdent::new(...)` or via the macro-emitted
    /// `<Module>::schema_ident()`).
    CheckProcessorSpecNew,

    /// CI gate for the cdylib-reachability invariant on engine `Host*`
    /// constructors. Fails when any constructor-class method
    /// (`new*` / `create*` / `from_*`) inside an `impl HostVulkan*`
    /// block in the engine RHI references `host_inner()` or
    /// `host_callbacks()` — those break the cdylib direct-call path
    /// documented at `docs/architecture/cdylib-reachability.md`.
    CheckCdylibReach,

    /// Regenerate `schemas/streamlib.schema.json` from the Rust
    /// [`StreamlibYaml`](streamlib_processor_schema::StreamlibYaml) source of
    /// truth (#714). Editors with `yaml-language-server` consume this schema
    /// for autocomplete + lint on every `streamlib.yaml`.
    EmitManifestSchema,

    /// CI gate for the streamlib.yaml schema (#714). Three assertions:
    /// (1) committed schema matches what Rust currently emits, (2) every
    /// `streamlib.yaml` carries the `# yaml-language-server: $schema=...`
    /// header, (3) every `streamlib.yaml` validates against the schema.
    CheckManifestSchema,

    /// Stage every in-tree workspace package at
    /// `target/streamlib-plugins/<org>__<name>/` so
    /// `Runner::load_workspace_packages` can resolve them by canonical
    /// id. Rust-impl packages get a cdylib build; schemas-only packages
    /// stage just their `streamlib.yaml` + `schemas/`. Defaults to the
    /// dev profile (faster inner loop); `--release` opts into the
    /// production-shaped profile.
    BuildPlugins {
        /// Use `--release` instead of the dev-loop default profile.
        #[arg(long)]
        release: bool,

        /// Build only these canonical-id packages (`@<org>/<name>`)
        /// instead of every Rust-impl package the workspace declares.
        /// Repeatable: `--package @tatolab/camera --package @tatolab/core`.
        #[arg(long = "package", value_name = "ORG/NAME")]
        packages: Vec<String>,
    },
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_target(false)
        .without_time()
        .init();

    let cli = Cli::parse();

    match cli.command {
        Commands::GenerateSchemas {
            runtime,
            output,
            project_dir,
            schema_file,
            schema_dir,
        } => generate(GenerateOptions {
            runtime,
            output,
            project_dir,
            schema_file,
            schema_dir,
            workspace_root: workspace_root()?,
            write_lockfile: true,
        })?,
        Commands::LintLogging => lint_logging::run(&workspace_root()?)?,
        Commands::CheckBoundaries => check_boundaries::run(&workspace_root()?)?,
        Commands::CheckSchemaVersions => check_schema_versions::run(&workspace_root()?)?,
        Commands::CheckNoStreamlibMetadata => {
            check_no_streamlib_metadata::run(&workspace_root()?)?
        }
        Commands::CheckNoReverseDns => check_no_reverse_dns::run(&workspace_root()?)?,
        Commands::CheckProcessorSpecNew => check_processor_spec_new::run(&workspace_root()?)?,
        Commands::CheckCdylibReach => check_cdylib_reach::run(&workspace_root()?)?,
        Commands::EmitManifestSchema => manifest_schema::emit(&workspace_root()?)?,
        Commands::CheckManifestSchema => manifest_schema::check(&workspace_root()?)?,
        Commands::BuildPlugins { release, packages } => {
            build_plugins::run(&workspace_root()?, release, &packages)?
        }
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
