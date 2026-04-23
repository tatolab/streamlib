// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! StreamLib CLI
//!
//! Command-line interface for spawning runtimes and managing local artifacts.

use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand};

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
    /// Package processors into a distributable .slpkg bundle
    Pack {
        /// Path to package directory (default: current directory)
        #[arg(value_name = "PACKAGE_DIR")]
        package_dir: Option<PathBuf>,

        /// Output file path (default: {name}-{version}.slpkg in package dir)
        #[arg(short, long)]
        output: Option<PathBuf>,
    },

    /// Stream a runtime's on-disk log file
    Logs {
        /// Runtime name or ID to stream logs from
        #[arg(long = "runtime", short = 'r')]
        runtime: String,

        /// Follow log output (like tail -f)
        #[arg(short = 'f', long)]
        follow: bool,

        /// Number of lines to show (default: 100)
        #[arg(short = 'n', long, default_value = "100")]
        lines: usize,

        /// Show logs since duration (e.g., "5m", "1h", "30s")
        #[arg(long)]
        since: Option<String>,
    },

    /// Run the StreamLib runtime
    Run {
        /// Pipeline graph file to load (JSON)
        #[arg(value_name = "GRAPH_FILE")]
        graph_file: Option<PathBuf>,

        /// Runtime name (auto-generated if not specified)
        #[arg(long)]
        name: Option<String>,

        /// Port for the API server (default: 9000)
        #[arg(short, long, default_value = "9000")]
        port: u16,

        /// Host address to bind to (default: 127.0.0.1)
        #[arg(long, default_value = "127.0.0.1")]
        host: String,

        /// Plugin libraries to load (can be specified multiple times)
        #[arg(long = "plugin", value_name = "PATH")]
        plugins: Vec<PathBuf>,

        /// Directory containing plugin libraries
        #[arg(long = "plugin-dir", value_name = "DIR")]
        plugin_dir: Option<PathBuf>,

        /// Run as a background daemon
        #[arg(short = 'd', long)]
        daemon: bool,
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
    /// Install a .slpkg package (local path or URL)
    Install {
        /// Path to .slpkg file or HTTP URL
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
    let _logging_guard = streamlib::logging::init(
        streamlib::logging::StreamlibLoggingConfig::for_cli("streamlib-cli"),
    )?;

    match cli.command {
        Some(Commands::Pack {
            package_dir,
            output,
        }) => {
            let dir = package_dir.unwrap_or_else(|| std::env::current_dir().unwrap());
            commands::pack::pack(&dir, output.as_deref())?;
        }
        Some(Commands::Logs {
            runtime,
            follow,
            lines,
            since,
        }) => commands::logs::stream(&runtime, follow, lines, since.as_deref()).await?,
        Some(Commands::Run {
            graph_file,
            name,
            port,
            host,
            plugins,
            plugin_dir,
            daemon,
        }) => {
            commands::serve::run(host, port, graph_file, plugins, plugin_dir, name, daemon)?;
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
            PkgCommands::Install { source } => commands::pkg::install(&source).await?,
            PkgCommands::Inspect { path } => commands::pkg::inspect(&path)?,
            PkgCommands::List => commands::pkg::list()?,
            PkgCommands::Remove { name } => commands::pkg::remove(&name)?,
        },
        None => {
            Cli::parse_from(["streamlib", "--help"]);
        }
    }

    Ok(())
}
