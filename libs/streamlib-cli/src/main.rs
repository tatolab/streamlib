// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! StreamLib CLI
//!
//! Command-line interface for managing StreamLib runtimes.

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

    /// Processor instances registered with the broker
    #[cfg(target_os = "macos")]
    Processors {
        #[command(subcommand)]
        action: ProcessorsCommands,
    },

    /// IOSurface management (GPU surfaces for cross-process sharing)
    #[cfg(target_os = "macos")]
    Surfaces {
        #[command(subcommand)]
        action: SurfacesCommands,
    },

    /// Broker service management (macOS only)
    #[cfg(target_os = "macos")]
    Broker {
        #[command(subcommand)]
        action: BrokerCommands,
    },

    /// Setup commands
    Setup {
        #[command(subcommand)]
        action: SetupCommands,
    },

    /// List and manage runtimes
    Runtimes {
        #[command(subcommand)]
        action: RuntimesCommands,
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

#[cfg(target_os = "macos")]
#[derive(Subcommand)]
enum SurfacesCommands {
    /// List registered IOSurfaces
    List {
        /// Filter by runtime name or ID
        #[arg(long = "runtime", short = 'r')]
        runtime: Option<String>,
    },
    /// Snapshot an IOSurface to a PNG file
    Snapshot {
        /// Surface ID (UUID) to snapshot
        #[arg(long)]
        id: String,

        /// Output file path for the PNG image
        #[arg(long, short = 'o', default_value = "snapshot.png")]
        output: std::path::PathBuf,
    },
}

#[cfg(target_os = "macos")]
#[derive(Subcommand)]
enum BrokerCommands {
    /// Install the broker service
    Install {
        /// Force reinstall even if already installed
        #[arg(long)]
        force: bool,

        /// Path to broker binary (default: find in target/release or PATH)
        #[arg(long)]
        binary: Option<std::path::PathBuf>,
    },

    /// Update the broker service (alias for install --force)
    Update {
        /// Path to broker binary (default: find in target/release or PATH)
        #[arg(long)]
        binary: Option<std::path::PathBuf>,
    },

    /// Uninstall the broker service
    Uninstall,

    /// Show broker health and version status
    Status,
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

#[cfg(target_os = "macos")]
#[derive(Subcommand)]
enum ProcessorsCommands {
    /// List processor instances
    List {
        /// Filter by runtime name or ID
        #[arg(long = "runtime", short = 'r')]
        runtime: Option<String>,
    },
}

#[derive(Subcommand)]
enum RuntimesCommands {
    /// List all registered runtimes
    List,
    /// Show detailed info about a runtime
    Describe {
        /// Runtime name or ID (queries broker for endpoint)
        #[arg(long = "runtime", short = 'r')]
        runtime: Option<String>,

        /// URL of the runtime API server (alternative to --runtime)
        #[arg(long)]
        url: Option<String>,
    },
    /// Show the graph of a runtime
    Graph {
        /// Runtime name or ID (queries broker for endpoint)
        #[arg(long = "runtime", short = 'r')]
        runtime: Option<String>,

        /// URL of the runtime API server (alternative to --runtime)
        #[arg(long)]
        url: Option<String>,

        /// Output format (json, dot, or pretty)
        #[arg(long, default_value = "pretty")]
        format: String,
    },
    /// Stream logs from a runtime
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
    /// Remove dead runtimes from the broker
    Prune,
}

#[derive(Subcommand)]
enum SchemasCommands {
    /// List all known schemas from a running runtime
    List {
        /// Runtime name or ID (queries broker for endpoint)
        #[arg(long = "runtime", short = 'r')]
        runtime: Option<String>,

        /// URL of the runtime API server (alternative to --runtime)
        #[arg(long)]
        url: Option<String>,
    },
    /// Show the YAML definition of a schema
    Describe {
        /// Schema name (e.g. com.tatolab.videoframe)
        name: String,

        /// Runtime name or ID (queries broker for endpoint)
        #[arg(long = "runtime", short = 'r')]
        runtime: Option<String>,

        /// URL of the runtime API server (alternative to --runtime)
        #[arg(long)]
        url: Option<String>,
    },
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
    /// List installed packages
    List,
    /// Remove an installed package
    Remove {
        /// Package name to remove
        name: String,
    },
}

fn main() -> Result<()> {
    // Load .env file if present (picks up STREAMLIB_BROKER_PORT, etc. in dev)
    let _ = dotenvy::dotenv();

    let cli = Cli::parse();

    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(async_main(cli))
}

async fn async_main(cli: Cli) -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".parse().unwrap()),
        )
        .init();

    match cli.command {
        Some(Commands::Pack {
            package_dir,
            output,
        }) => {
            let dir = package_dir.unwrap_or_else(|| std::env::current_dir().unwrap());
            commands::pack::pack(&dir, output.as_deref())?;
        }
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
        #[cfg(target_os = "macos")]
        Some(Commands::Processors { action }) => match action {
            ProcessorsCommands::List { runtime } => {
                commands::broker::processors(runtime.as_deref()).await?
            }
        },
        #[cfg(target_os = "macos")]
        Some(Commands::Surfaces { action }) => match action {
            SurfacesCommands::List { runtime } => {
                commands::broker::surfaces(runtime.as_deref()).await?
            }
            SurfacesCommands::Snapshot { id, output } => {
                commands::broker::snapshot(&id, &output).await?
            }
        },
        #[cfg(target_os = "macos")]
        Some(Commands::Broker { action }) => match action {
            BrokerCommands::Install { force, binary } => {
                commands::broker::install(force, binary.as_deref()).await?
            }
            BrokerCommands::Update { binary } => {
                commands::broker::install(true, binary.as_deref()).await?
            }
            BrokerCommands::Uninstall => commands::broker::uninstall().await?,
            BrokerCommands::Status => commands::broker::status().await?,
        },
        Some(Commands::Setup { action }) => match action {
            SetupCommands::Shell { shell } => commands::setup::shell(shell.as_deref())?,
        },
        Some(Commands::Runtimes { action }) => match action {
            RuntimesCommands::List => commands::runtimes::list().await?,
            RuntimesCommands::Describe { runtime, url } => {
                commands::inspect::run(runtime.as_deref(), url.as_deref()).await?
            }
            RuntimesCommands::Graph {
                runtime,
                url,
                format,
            } => commands::inspect::graph(runtime.as_deref(), url.as_deref(), &format).await?,
            RuntimesCommands::Logs {
                runtime,
                follow,
                lines,
                since,
            } => commands::logs::stream(&runtime, follow, lines, since.as_deref()).await?,
            RuntimesCommands::Prune => commands::runtimes::prune().await?,
        },
        Some(Commands::Schemas { action }) => match action {
            SchemasCommands::List { runtime, url } => {
                commands::schema::list(runtime.as_deref(), url.as_deref()).await?
            }
            SchemasCommands::Describe { name, runtime, url } => {
                commands::schema::describe(&name, runtime.as_deref(), url.as_deref()).await?
            }
            SchemasCommands::ValidateProcessor { path } => {
                commands::schema::validate_processor(&path)?
            }
        },
        Some(Commands::Pkg { action }) => match action {
            PkgCommands::Install { source } => commands::pkg::install(&source).await?,
            PkgCommands::List => commands::pkg::list()?,
            PkgCommands::Remove { name } => commands::pkg::remove(&name)?,
        },
        None => {
            // No subcommand: show help
            Cli::parse_from(["streamlib", "--help"]);
        }
    }

    Ok(())
}
