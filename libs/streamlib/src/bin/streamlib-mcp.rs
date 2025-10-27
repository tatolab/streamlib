//! streamlib MCP Server Binary
//!
//! This binary starts an MCP server that exposes streamlib processors
//! to AI agents like Claude Code.
//!
//! Runs in **application mode** with a live StreamRuntime, enabling AI agents to:
//! - List running processors (list_processors)
//! - Remove processors dynamically (remove_processor)
//! - List connections (list_connections)
//! - Discover available processor types (discovery mode features)
//!
//! Usage:
//! ```bash
//! # Install the binary
//! cargo install streamlib --features mcp
//!
//! # Stdio transport (default, for Claude Desktop integration)
//! streamlib-mcp
//!
//! # HTTP transport (for remote MCP tools, defaults to port 3050)
//! streamlib-mcp --http
//! # Or specify a custom port:
//! streamlib-mcp --http --port 3060
//! ```

use streamlib::{global_registry, mcp::McpServer};
use streamlib::core::StreamRuntime;
use clap::Parser;
use std::sync::Arc;
use tokio::sync::Mutex as TokioMutex;

#[derive(Parser, Debug)]
#[command(name = "streamlib-mcp")]
#[command(about = "MCP server for streamlib processors")]
#[command(version)]
struct Args {
    /// Use HTTP transport instead of stdio
    #[arg(long)]
    http: bool,

    /// Port to bind to (HTTP mode only)
    #[arg(long, default_value = "3050")]
    port: u16,

    /// Host to bind to (HTTP mode only)
    #[arg(long, default_value = "127.0.0.1")]
    host: String,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Parse CLI arguments
    let args = Args::parse();

    // Initialize tracing (logs to stderr so they don't interfere with stdio MCP protocol)
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .init();

    tracing::info!("Starting streamlib MCP server");

    // Get the global processor registry
    // Built-in processors (CameraProcessor, DisplayProcessor) are automatically registered
    // via inventory at compile-time. Any additional processors submitted with inventory::submit!
    // will also be registered automatically.
    let registry = global_registry();

    tracing::info!("Auto-registered {} processors from inventory", {
        let reg = registry.lock().unwrap();
        reg.list().len()
    });

    // Create and start StreamRuntime (application mode)
    tracing::info!("Creating StreamRuntime at 60 FPS");
    let mut runtime = StreamRuntime::new(60.0);

    // Start the runtime
    tracing::info!("Starting StreamRuntime...");
    runtime.start().await?;
    tracing::info!("StreamRuntime started successfully");

    // Wrap runtime in Arc<TokioMutex<>> for MCP server
    let runtime = Arc::new(TokioMutex::new(runtime));

    // Create MCP server in APPLICATION MODE (with runtime control)
    let server = McpServer::with_runtime(registry.clone(), runtime.clone());

    tracing::info!(
        "MCP server {} v{} ready (APPLICATION MODE - runtime control enabled)",
        server.name(),
        server.version()
    );

    // Run the server with the selected transport
    // The runtime will keep running in the background while the MCP server handles requests
    if args.http {
        let bind_addr = format!("{}:{}", args.host, args.port);
        tracing::info!("Using HTTP transport on {}", bind_addr);
        server.run_http(&bind_addr).await?;
    } else {
        tracing::info!("Using stdio transport (for Claude Desktop integration)");
        // This will handle JSON-RPC messages from stdin and write responses to stdout
        server.run_stdio().await?;
    }

    // Cleanup: Stop the runtime when MCP server exits
    tracing::info!("Stopping StreamRuntime...");
    let mut rt = runtime.lock().await;
    rt.stop().await?;
    tracing::info!("StreamRuntime stopped");

    Ok(())
}
