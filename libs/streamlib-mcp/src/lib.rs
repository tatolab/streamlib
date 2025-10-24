//! streamlib-mcp: MCP Server Integration for AI Agents
//!
//! Provides Model Context Protocol (MCP) server functionality for streamlib,
//! enabling AI agents to discover and interact with streaming processors.
//!
//! ## Architecture
//!
//! - **Resources**: Processor descriptors (read-only capability discovery)
//! - **Tools**: Actions like add_processor, connect_processors (runtime operations)
//!
//! ## Usage
//!
//! ```no_run
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! use streamlib_mcp::McpServer;
//! use streamlib_core::global_registry;
//!
//! let server = McpServer::new(global_registry());
//! server.run_stdio().await?;
//! # Ok(())
//! # }
//! ```

mod error;
mod resources;
mod tools;

pub use error::{McpError, Result};

use streamlib_core::ProcessorRegistry;
use std::sync::{Arc, Mutex};

/// MCP Server for streamlib
///
/// Exposes processor registry via MCP protocol for AI agent integration.
/// Supports both stdio and HTTP transports.
pub struct McpServer {
    /// Processor registry (shared with runtime)
    registry: Arc<Mutex<ProcessorRegistry>>,

    /// Server name for MCP identification
    name: String,

    /// Server version
    version: String,
}

impl McpServer {
    /// Create a new MCP server
    ///
    /// # Arguments
    /// * `registry` - Shared processor registry
    pub fn new(registry: Arc<Mutex<ProcessorRegistry>>) -> Self {
        Self {
            registry,
            name: "streamlib-mcp".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
        }
    }

    /// Create with custom name and version
    pub fn with_info(
        registry: Arc<Mutex<ProcessorRegistry>>,
        name: impl Into<String>,
        version: impl Into<String>,
    ) -> Self {
        Self {
            registry,
            name: name.into(),
            version: version.into(),
        }
    }

    /// Run MCP server over stdio (for Claude Desktop, etc.)
    ///
    /// This is the primary mode for local AI agents.
    /// Communication happens via stdin/stdout.
    pub async fn run_stdio(&self) -> Result<()> {
        tracing::info!("Starting MCP server on stdio");

        // TODO: Implement MCP stdio server using mcp-rs SDK
        // This will handle JSON-RPC messages over stdin/stdout

        Err(McpError::NotImplemented(
            "stdio transport not yet implemented".into()
        ))
    }

    /// Run MCP server over HTTP
    ///
    /// This mode is useful for remote AI agents or distributed systems.
    ///
    /// # Arguments
    /// * `bind_addr` - Address to bind to (e.g., "127.0.0.1:3000")
    pub async fn run_http(&self, _bind_addr: &str) -> Result<()> {
        tracing::info!("Starting MCP server on HTTP");

        // TODO: Implement MCP HTTP server using mcp-rs SDK

        Err(McpError::NotImplemented(
            "HTTP transport not yet implemented".into()
        ))
    }

    /// Get server name
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Get server version
    pub fn version(&self) -> &str {
        &self.version
    }

    /// Get processor registry
    pub fn registry(&self) -> Arc<Mutex<ProcessorRegistry>> {
        Arc::clone(&self.registry)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use streamlib_core::ProcessorRegistry;

    #[test]
    fn test_create_server() {
        let registry = Arc::new(Mutex::new(ProcessorRegistry::new()));
        let server = McpServer::new(registry);

        assert_eq!(server.name(), "streamlib-mcp");
        assert!(!server.version().is_empty());
    }

    #[test]
    fn test_custom_info() {
        let registry = Arc::new(Mutex::new(ProcessorRegistry::new()));
        let server = McpServer::with_info(
            registry,
            "my-custom-server",
            "1.2.3",
        );

        assert_eq!(server.name(), "my-custom-server");
        assert_eq!(server.version(), "1.2.3");
    }
}
