// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! StreamLib CLI
//!
//! Command-line interface for managing StreamLib runtimes.

use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand};

mod commands;
mod plugin_loader;

#[derive(Parser)]
#[command(name = "streamlib")]
#[command(author, version, about = "StreamLib runtime CLI", long_about = None)]
struct Cli {
    /// Pipeline graph file to load (JSON)
    #[arg(value_name = "GRAPH_FILE")]
    graph_file: Option<PathBuf>,

    /// Port for the API server (default: 9000)
    #[arg(short, long, default_value = "9000")]
    port: u16,

    /// Host address to bind to (default: 127.0.0.1)
    #[arg(long, default_value = "127.0.0.1")]
    host: String,

    /// Disable API server
    #[arg(long)]
    no_api: bool,

    /// Plugin libraries to load (can be specified multiple times)
    #[arg(long = "plugin", value_name = "PATH")]
    plugins: Vec<PathBuf>,

    /// Directory containing plugin libraries
    #[arg(long = "plugin-dir", value_name = "DIR")]
    plugin_dir: Option<PathBuf>,

    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
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
        /// URL of the runtime API server
        #[arg(long, default_value = "http://127.0.0.1:9000")]
        url: String,

        /// Output format (json, dot, or pretty)
        #[arg(long, default_value = "pretty")]
        format: String,
    },

    /// XPC broker diagnostics (macOS only)
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

    /// MCP server for Claude Code integration (macOS only)
    #[cfg(target_os = "macos")]
    Mcp {
        #[command(subcommand)]
        action: McpCommands,
    },
}

#[derive(Subcommand)]
enum ListCommands {
    /// List all available processor types
    Processors,

    /// List all available schemas
    Schemas,
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

    /// List active connections
    Connections {
        /// Filter by runtime ID
        #[arg(long)]
        runtime: Option<String>,
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

#[cfg(target_os = "macos")]
#[derive(Subcommand)]
enum McpCommands {
    /// Start MCP server on stdio (for Claude Code integration)
    Serve,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".parse().unwrap()),
        )
        .init();

    let cli = Cli::parse();

    match cli.command {
        Some(Commands::List { what }) => match what {
            ListCommands::Processors => commands::list::processors()?,
            ListCommands::Schemas => commands::list::schemas()?,
        },
        Some(Commands::Inspect { url }) => {
            commands::inspect::run(&url).await?;
        }
        Some(Commands::Graph { url, format }) => {
            commands::inspect::graph(&url, &format).await?;
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
            BrokerCommands::Connections { runtime } => {
                commands::broker::connections(runtime.as_deref()).await?
            }
        },
        Some(Commands::Setup { action }) => match action {
            SetupCommands::Shell { shell } => commands::setup::shell(shell.as_deref())?,
        },
        #[cfg(target_os = "macos")]
        Some(Commands::Mcp { action }) => match action {
            McpCommands::Serve => commands::mcp::serve().await?,
        },
        // No subcommand: start runtime (default behavior)
        None => {
            commands::serve::run(
                cli.host,
                cli.port,
                cli.no_api,
                cli.graph_file,
                cli.plugins,
                cli.plugin_dir,
            )
            .await?;
        }
    }

    Ok(())
}
