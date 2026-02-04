// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! StreamLib CLI
//!
//! Command-line interface for managing StreamLib runtimes.

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

mod commands;
mod plugin_loader;

/// Daemonize the process before entering async context.
/// Must be called before tokio runtime is created.
#[cfg(unix)]
fn daemonize_if_requested(name: &str, port: u16, host: &str) -> Result<()> {
    use daemonize::Daemonize;

    let home =
        dirs::home_dir().ok_or_else(|| anyhow::anyhow!("Could not determine home directory"))?;
    let logs_dir = home.join(".streamlib").join("logs");
    let pids_dir = home.join(".streamlib").join("pids");

    std::fs::create_dir_all(&logs_dir)?;
    std::fs::create_dir_all(&pids_dir)?;

    let pid_path = pids_dir.join(format!("{}.pid", name));

    // Print before forking (after fork, stdout goes to /dev/null)
    println!("runtime/{} started", name);
    println!("  API: http://{}:{}", host, port);
    println!();
    println!("Next steps:");
    println!("  streamlib logs -r {} -f", name);
    println!("  streamlib runtimes list");

    let daemonize = Daemonize::new()
        .pid_file(&pid_path)
        .working_directory(std::env::current_dir()?);

    daemonize.start().context("Failed to daemonize")?;

    Ok(())
}

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

    /// List available processors or schemas
    List {
        #[command(subcommand)]
        what: ListCommands,
    },

    /// Inspect a running StreamLib runtime
    Inspect {
        /// URL of the runtime API server
        #[arg(long, default_value = "http://127.0.0.1:9000")]
        url: String,
    },

    /// Show the graph of a running runtime
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

    /// Broker diagnostics (macOS only)
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
enum ListCommands {
    /// List all available processor types
    Processors {
        /// Runtime name or ID (queries broker for endpoint)
        #[arg(long = "runtime", short = 'r')]
        runtime: Option<String>,

        /// URL of the runtime API server (alternative to --runtime)
        #[arg(long)]
        url: Option<String>,
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

    /// List registered runtimes
    Runtimes,

    /// List registered processors
    Processors {
        /// Filter by runtime ID
        #[arg(long)]
        runtime: Option<String>,
    },

    /// List registered IOSurfaces (GPU surfaces for cross-process sharing)
    Surfaces {
        /// Filter by runtime ID
        #[arg(long)]
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
enum RuntimesCommands {
    /// List all registered runtimes
    List,
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
    Get {
        /// Schema name (e.g. com.tatolab.videoframe)
        name: String,
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
    let cli = Cli::parse();

    // Handle daemon mode BEFORE creating tokio runtime
    // Forking after tokio starts corrupts its internal state
    #[cfg(unix)]
    if let Some(Commands::Run {
        daemon: true,
        ref name,
        port,
        ref host,
        ..
    }) = cli.command
    {
        // Generate name now if not provided (need it for daemonize output)
        let runtime_name = name
            .clone()
            .unwrap_or_else(commands::serve::generate_runtime_name);
        // Store the generated name back for the async code
        std::env::set_var("_STREAMLIB_DAEMON_NAME", &runtime_name);
        daemonize_if_requested(&runtime_name, port, host)?;
    }

    // Now create tokio runtime and run async main
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(async_main(cli))
}

async fn async_main(cli: Cli) -> Result<()> {
    // Initialize tracing for non-Run commands (Run command sets up its own file-based logging)
    let is_run_command = matches!(cli.command, Some(Commands::Run { .. }));
    if !is_run_command {
        tracing_subscriber::fmt()
            .with_env_filter(
                tracing_subscriber::EnvFilter::try_from_default_env()
                    .unwrap_or_else(|_| "info".parse().unwrap()),
            )
            .init();
    }

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
            // If daemon mode, use the pre-generated name from env var
            let actual_name = if daemon {
                std::env::var("_STREAMLIB_DAEMON_NAME").ok().or(name)
            } else {
                name
            };
            commands::serve::run(
                host,
                port,
                graph_file,
                plugins,
                plugin_dir,
                actual_name,
                daemon,
            )
            .await?;
        }
        Some(Commands::List { what }) => match what {
            ListCommands::Processors { runtime, url } => {
                commands::list::processors(runtime.as_deref(), url.as_deref()).await?
            }
        },
        Some(Commands::Inspect { url }) => {
            commands::inspect::run(&url).await?;
        }
        Some(Commands::Graph {
            runtime,
            url,
            format,
        }) => {
            commands::inspect::graph(runtime.as_deref(), url.as_deref(), &format).await?;
        }
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
            BrokerCommands::Runtimes => commands::broker::runtimes().await?,
            BrokerCommands::Processors { runtime } => {
                commands::broker::processors(runtime.as_deref()).await?
            }
            BrokerCommands::Surfaces { runtime } => {
                commands::broker::surfaces(runtime.as_deref()).await?
            }
            BrokerCommands::Snapshot { id, output } => {
                commands::broker::snapshot(&id, &output).await?
            }
        },
        Some(Commands::Setup { action }) => match action {
            SetupCommands::Shell { shell } => commands::setup::shell(shell.as_deref())?,
        },
        Some(Commands::Runtimes { action }) => match action {
            RuntimesCommands::List => commands::runtimes::list().await?,
            RuntimesCommands::Prune => commands::runtimes::prune().await?,
        },
        Some(Commands::Logs {
            runtime,
            follow,
            lines,
            since,
        }) => {
            commands::logs::stream(&runtime, follow, lines, since.as_deref()).await?;
        }
        Some(Commands::Schemas { action }) => match action {
            SchemasCommands::List { runtime, url } => {
                commands::schema::list(runtime.as_deref(), url.as_deref()).await?
            }
            SchemasCommands::Get { name } => commands::schema::get(&name)?,
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
