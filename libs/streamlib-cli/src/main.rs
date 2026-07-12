// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! StreamLib CLI
//!
//! Command-line interface for spawning runtimes and managing local artifacts.

use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand};
use streamlib_jtd_codegen::RuntimeTarget;

mod commands;

#[derive(Parser)]
#[command(name = "streamlib")]
#[command(author, version, about = "StreamLib runtime CLI", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Stream a runtime's on-disk JSONL log file in pretty format.
    Logs {
        /// Runtime ID to read logs for. Omit when using `--list`.
        #[arg(value_name = "RUNTIME_ID", required_unless_present = "list")]
        runtime_id: Option<String>,

        /// Enumerate available runtime log files instead of streaming one.
        #[arg(long, conflicts_with_all = ["runtime_id", "follow"])]
        list: bool,

        /// Follow the log file as new records land (like `tail -F`).
        #[arg(short = 'f', long)]
        follow: bool,

        /// Filter by processor ID.
        #[arg(long, value_name = "ID")]
        processor: Option<String>,

        /// Filter by pipeline ID.
        #[arg(long, value_name = "ID")]
        pipeline: Option<String>,

        /// Show only RHI operations (records with `rhi_op` set).
        #[arg(long)]
        rhi: bool,

        /// Filter by minimum severity level.
        #[arg(long, value_name = "LEVEL", value_parser = ["trace", "debug", "info", "warn", "error"])]
        level: Option<String>,

        /// Filter by source runtime.
        #[arg(long, value_name = "SOURCE", value_parser = ["rust", "python", "deno"])]
        source: Option<String>,

        /// Show only intercepted records (captured stdout/stderr/print/console.log).
        #[arg(long = "intercepted-only")]
        intercepted_only: bool,

        /// (Orchestrator-only) Show logs since a duration ago. Not supported in offline mode.
        #[arg(long, value_name = "DURATION")]
        since: Option<String>,
    },

    /// Setup commands
    Setup {
        #[command(subcommand)]
        action: SetupCommands,
    },

    /// Schema management
    Schemas {
        #[command(subcommand)]
        action: SchemasCommands,
    },

    /// Manage installed packages
    Pkg {
        #[command(subcommand)]
        action: PkgCommands,
    },

    /// Point this consumer's entire streamlib surface at a local checkout.
    ///
    /// Run from a consumer root (app or package dir). Emits whole-tree
    /// language-native overrides (cargo `[patch]`, uv sources, Deno import map)
    /// so an edit in the checkout is picked up by the next build with no
    /// publish. Omit `<CHECKOUT>` to print the active link status.
    Link {
        /// Path to a local streamlib checkout. Omit to print status.
        checkout: Option<PathBuf>,

        /// Skip the post-link cargo resolution verification.
        #[arg(long)]
        skip_verify: bool,
    },

    /// Remove the active streamlib link, restoring every manifest byte-identically.
    Unlink {
        /// Discard files modified while the link was active instead of refusing.
        #[arg(long)]
        force: bool,
    },

    /// Generate typed bindings from JTD schemas via the JTD-codegen pipeline.
    ///
    /// Same pipeline contributors run as `cargo xtask generate-schemas`,
    /// reachable here without rustup.
    Generate {
        /// Target language (default: rust)
        #[arg(long, default_value = "rust")]
        runtime: RuntimeTarget,

        /// Output directory (required)
        #[arg(long)]
        output: PathBuf,

        /// `streamlib.yaml`-driven mode: directory containing the manifest.
        /// The resolver walks declared dependencies and codegen ingests the
        /// resulting set, writing `streamlib.lock` next to the manifest.
        #[arg(long, group = "input")]
        project_dir: Option<PathBuf>,

        /// Process a single schema file
        #[arg(long, group = "input")]
        schema_file: Option<PathBuf>,

        /// Process all .yaml files in a directory
        #[arg(long, group = "input")]
        schema_dir: Option<PathBuf>,
    },
}

#[derive(Subcommand)]
enum SetupCommands {
    /// Configure shell to add streamlib to PATH
    Shell {
        /// Shell type (bash, zsh, fish). Auto-detected if not specified.
        #[arg(long)]
        shell: Option<String>,
    },
}

#[derive(Subcommand)]
enum SchemasCommands {
    /// Validate a processor YAML schema file
    ValidateProcessor {
        /// Path to the processor YAML file
        path: PathBuf,
    },
}

#[derive(Subcommand)]
enum PkgCommands {
    /// Build THIS package into a source-only `.slpkg` (run inside the package).
    ///
    /// Bundles source only — no compilation, no prebuilt cdylib, nothing
    /// path-related. The consumer builds it from source on their host
    /// (`pkg install` / runtime registry resolution), pulling every dep
    /// from the registry. The artifact is for hand-off (email it, hand it to
    /// a runtime); `publish` repacks independently.
    Build {
        /// Output file path (default: {name}-{version}.slpkg in the package dir)
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
    /// Publish THIS package to the registry (run inside the package).
    ///
    /// Always repacks a fresh source-only `.slpkg` (never trusts an existing
    /// artifact) and uploads it to the Gitea generic registry. Registry
    /// endpoint + token come from `STREAMLIB_REGISTRY_URL` (or `GITEA_URL`)
    /// and `STREAMLIB_REGISTRY_TOKEN`. Publishing many packages is a script
    /// over this single-package command.
    Publish,
    /// Remove THIS package's build/pack artifacts (run inside the package):
    /// any `*.slpkg`, the prebuilt `lib/` dir, and generated `_generated_/` trees.
    Clean,
    /// Install a package: a registry ref `@org/name[@version]` (resolved from
    /// the registry and built from source), a local `.slpkg` path, or an HTTP URL.
    Install {
        /// `@org/name[@version]` | path to a `.slpkg` | HTTP URL
        source: String,
    },
    /// Inspect a .slpkg package (show manifest without installing)
    Inspect {
        /// Path to .slpkg file
        path: PathBuf,
    },
    /// List installed packages
    List,
    /// Remove an installed package
    Remove {
        /// Package name to remove
        name: String,
    },
}

fn main() -> Result<()> {
    let _ = dotenvy::dotenv();

    let cli = Cli::parse();

    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(async_main(cli))
}

async fn async_main(cli: Cli) -> Result<()> {
    // Short-lived CLI invocation: stdout-only tracing, no JSONL file.
    let _logging_guard = streamlib::sdk::logging::init(
        streamlib::sdk::logging::StreamlibLoggingConfig::for_cli("streamlib-cli"),
    )?;

    match cli.command {
        Some(Commands::Logs {
            runtime_id,
            list,
            follow,
            processor,
            pipeline,
            rhi,
            level,
            source,
            intercepted_only,
            since,
        }) => {
            commands::logs::run(commands::logs::LogsArgs {
                runtime_id: runtime_id.as_deref(),
                list,
                follow,
                processor: processor.as_deref(),
                pipeline: pipeline.as_deref(),
                rhi,
                level: level.as_deref(),
                source: source.as_deref(),
                intercepted_only,
                since: since.as_deref(),
            })
            .await?
        }
        Some(Commands::Setup { action }) => match action {
            SetupCommands::Shell { shell } => commands::setup::shell(shell.as_deref())?,
        },
        Some(Commands::Schemas { action }) => match action {
            SchemasCommands::ValidateProcessor { path } => {
                commands::schema::validate_processor(&path)?
            }
        },
        Some(Commands::Pkg { action }) => match action {
            PkgCommands::Build { output } => commands::pkg::build(output.as_deref())?,
            PkgCommands::Publish => commands::pkg::publish()?,
            PkgCommands::Clean => commands::pkg::clean()?,
            PkgCommands::Install { source } => commands::pkg::install(&source).await?,
            PkgCommands::Inspect { path } => commands::pkg::inspect(&path)?,
            PkgCommands::List => commands::pkg::list()?,
            PkgCommands::Remove { name } => commands::pkg::remove(&name)?,
        },
        Some(Commands::Link {
            checkout,
            skip_verify,
        }) => {
            let consumer_root = std::env::current_dir()?;
            match checkout {
                Some(checkout) => commands::link::link(&consumer_root, &checkout, skip_verify)?,
                None => commands::link::status(&consumer_root)?,
            }
        }
        Some(Commands::Unlink { force }) => {
            let consumer_root = std::env::current_dir()?;
            commands::link::unlink(&consumer_root, force)?;
        }
        Some(Commands::Generate {
            runtime,
            output,
            project_dir,
            schema_file,
            schema_dir,
        }) => commands::generate::run(runtime, output, project_dir, schema_file, schema_dir)?,
        None => {
            Cli::parse_from(["streamlib", "--help"]);
        }
    }

    Ok(())
}
