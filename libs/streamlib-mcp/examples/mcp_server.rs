//! MCP Server Example
//!
//! This example starts an MCP server over stdio that exposes streamlib processors
//! to AI agents like Claude Code.
//!
//! Usage:
//! ```bash
//! cargo run --example mcp_server
//! ```

use streamlib_core::global_registry;
use streamlib_mcp::McpServer;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
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

    // Run the server over stdio
    // This will handle JSON-RPC messages from stdin and write responses to stdout
    server.run_stdio().await?;

    Ok(())
}
