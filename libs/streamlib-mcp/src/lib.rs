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
use std::future::Future;

// MCP protocol imports
use rmcp::{
    model::*,
    ServerHandler, ServiceExt, RoleServer,
    service::RequestContext,
    transport::stdio,
    ErrorData as RmcpError,
};

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

        // Create handler with registry
        let handler = StreamlibMcpHandler {
            registry: Arc::clone(&self.registry),
            name: self.name.clone(),
            version: self.version.clone(),
        };

        // Start stdio server
        let service = handler.serve(stdio()).await
            .map_err(|e| McpError::Protocol(format!("Failed to start stdio server: {}", e)))?;

        tracing::info!("MCP server running on stdio, awaiting requests");

        // Wait for completion
        service.waiting().await
            .map_err(|e| McpError::Protocol(format!("Server error: {}", e)))?;

        Ok(())
    }

    /// Run MCP server over HTTP
    ///
    /// This mode is useful for remote AI agents or distributed systems.
    ///
    /// # Arguments
    /// * `bind_addr` - Address to bind to (e.g., "127.0.0.1:3000")
    pub async fn run_http(&self, _bind_addr: &str) -> Result<()> {
        tracing::info!("Starting MCP server on HTTP");

        // TODO: Implement HTTP transport when rmcp supports it
        // For now, HTTP is not prioritized - stdio is the main transport for Claude Desktop

        Err(McpError::NotImplemented(
            "HTTP transport not yet implemented - use stdio() for now".into()
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

/// Internal MCP handler that implements the protocol
#[derive(Clone)]
struct StreamlibMcpHandler {
    registry: Arc<Mutex<ProcessorRegistry>>,
    name: String,
    version: String,
}

// Implement ServerHandler for MCP protocol
impl ServerHandler for StreamlibMcpHandler {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            protocol_version: ProtocolVersion::V_2024_11_05,
            capabilities: ServerCapabilities::builder()
                .enable_resources()
                .enable_tools()
                .build(),
            server_info: Implementation {
                name: self.name.clone(),
                version: self.version.clone(),
                title: None,
                icons: None,
                website_url: None,
            },
            instructions: Some(
                "streamlib MCP server - discover and interact with streaming processors".to_string()
            ),
        }
    }

    // Implement resource listing
    fn list_resources(
        &self,
        _params: Option<PaginatedRequestParam>,
        _ctx: RequestContext<RoleServer>,
    ) -> impl Future<Output = std::result::Result<ListResourcesResult, RmcpError>> + Send {
        let registry = Arc::clone(&self.registry);

        async move {
            tracing::debug!("MCP: list_resources called");

            let resources = resources::list_resources(registry)
                .map_err(|e| RmcpError::internal_error(
                    "list_resources_error",
                    Some(serde_json::json!({"error": e.to_string()}))
                ))?;

            // Convert our Resource type to rmcp's Resource type
            let resources: Vec<Resource> = resources
                .into_iter()
                .map(|r| {
                    let raw = RawResource {
                        uri: r.uri.into(),
                        name: r.name.into(),
                        description: Some(r.description.into()),
                        mime_type: Some(r.mime_type.into()),
                        title: None,
                        icons: None,
                        size: None,
                    };
                    Resource::new(raw, None)
                })
                .collect();

            Ok(ListResourcesResult {
                resources,
                next_cursor: None,
            })
        }
    }

    // Implement resource reading
    fn read_resource(
        &self,
        params: ReadResourceRequestParam,
        _ctx: RequestContext<RoleServer>,
    ) -> impl Future<Output = std::result::Result<ReadResourceResult, RmcpError>> + Send {
        let registry = Arc::clone(&self.registry);

        async move {
            tracing::debug!("MCP: read_resource called for URI: {}", params.uri);

            let content = resources::read_resource(registry, &params.uri)
                .map_err(|e| RmcpError::internal_error(
                    "resource_read_error",
                    Some(serde_json::json!({"error": e.to_string(), "uri": params.uri}))
                ))?;

            Ok(ReadResourceResult {
                contents: vec![ResourceContents::text(
                    content.text,
                    content.uri,
                )],
            })
        }
    }

    // Implement tool listing
    fn list_tools(
        &self,
        _params: Option<PaginatedRequestParam>,
        _ctx: RequestContext<RoleServer>,
    ) -> impl Future<Output = std::result::Result<ListToolsResult, RmcpError>> + Send {
        async move {
            tracing::debug!("MCP: list_tools called");

            let tools = tools::list_tools();

            // Convert to rmcp Tool type
            let tools: Vec<Tool> = tools
                .into_iter()
                .map(|t| {
                    // Convert JSON Value to Arc<Map>
                    let input_schema = if let serde_json::Value::Object(map) = t.input_schema {
                        Arc::new(map)
                    } else {
                        Arc::new(serde_json::Map::new())
                    };

                    Tool::new(
                        t.name,
                        t.description,
                        input_schema,
                    )
                })
                .collect();

            Ok(ListToolsResult {
                tools,
                next_cursor: None,
            })
        }
    }

    // Implement tool calling
    fn call_tool(
        &self,
        params: CallToolRequestParam,
        _ctx: RequestContext<RoleServer>,
    ) -> impl Future<Output = std::result::Result<CallToolResult, RmcpError>> + Send {
        async move {
            tracing::debug!("MCP: call_tool called: {}", params.name);

            // Convert arguments from Map to Value
            let arguments = serde_json::Value::Object(params.arguments.unwrap_or_default());

            let result = tools::execute_tool(&params.name, arguments).await
                .map_err(|e| RmcpError::internal_error(
                    "tool_execution_error",
                    Some(serde_json::json!({"error": e.to_string()}))
                ))?;

            let mut contents = vec![Content::text(result.message)];
            if let Some(data) = result.data {
                contents.push(Content::text(serde_json::to_string_pretty(&data).unwrap()));
            }

            Ok(CallToolResult {
                content: contents,
                is_error: Some(!result.success),
                structured_content: None,
                meta: None,
            })
        }
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
