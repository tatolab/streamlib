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
}

#[derive(Subcommand)]
enum ListCommands {
    /// List all available processor types
    Processors,

    /// List all available schemas
    Schemas,
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
