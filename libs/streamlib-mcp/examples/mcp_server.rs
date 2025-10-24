//! MCP Server Example
//!
//! This example starts an MCP server that exposes streamlib processors
//! to AI agents like Claude Code.
//!
//! Usage:
//! ```bash
//! # Stdio transport (default, for Claude Desktop integration)
//! cargo run --example mcp_server
//!
//! # HTTP transport (for remote MCP tools, defaults to port 3050)
//! cargo run --example mcp_server -- --http
//! # Or specify a custom port:
//! cargo run --example mcp_server -- --http --port 3060
//! ```

use streamlib_core::global_registry;
use streamlib_mcp::McpServer;
use clap::Parser;

#[derive(Parser, Debug)]
#[command(name = "streamlib-mcp-server")]
#[command(about = "MCP server for streamlib processors")]
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

    // Create MCP server with the shared registry
    let server = McpServer::new(registry);

    tracing::info!(
        "MCP server {} v{} ready",
        server.name(),
        server.version()
    );

    // Run the server with the selected transport
    if args.http {
        let bind_addr = format!("{}:{}", args.host, args.port);
        tracing::info!("Using HTTP transport on {}", bind_addr);
        server.run_http(&bind_addr).await?;
    } else {
        tracing::info!("Using stdio transport (for Claude Desktop integration)");
        // This will handle JSON-RPC messages from stdin and write responses to stdout
        server.run_stdio().await?;
    }

    Ok(())
}
