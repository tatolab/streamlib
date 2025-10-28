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
//! use crate::core::global_registry;
//!
//! let server = McpServer::new(global_registry());
//! server.run_stdio().await?;
//! # Ok(())
//! # }
//! ```

mod error;
mod resources;
mod tools;
mod package_manager;

pub use error::{McpError, Result};
pub use package_manager::{PackageManager, PackageInfo, PackageStatus, ApprovalPolicy};

use crate::core::{ProcessorRegistry, StreamRuntime};
use std::sync::{Arc, Mutex};
use std::future::Future;
use std::collections::HashSet;
use tokio::sync::Mutex as TokioMutex;

// MCP protocol imports
use rmcp::{
    model::*,
    ServerHandler, ServiceExt, RoleServer,
    service::RequestContext,
    transport::{
        stdio,
        streamable_http_server::{
            StreamableHttpService, StreamableHttpServerConfig,
            session::local::LocalSessionManager,
        },
    },
    ErrorData as RmcpError,
};

/// MCP Server for streamlib
///
/// Exposes processor registry and runtime control via MCP protocol for AI agent integration.
/// Supports both stdio and HTTP transports.
///
/// # Modes
///
/// - **Discovery Mode** (registry only): `McpServer::new(registry)` - AI agents can discover processor types
/// - **Application Mode** (registry + runtime): `McpServer::with_runtime(registry, runtime)` - AI agents can control running system
pub struct McpServer {
    /// Processor registry (shared with runtime)
    registry: Arc<Mutex<ProcessorRegistry>>,

    /// Optional runtime for live system control
    /// If None, MCP server is in "discovery mode" (can only list processor types)
    /// If Some, MCP server is in "application mode" (can add/remove processors, list connections, etc.)
    /// Uses tokio::sync::Mutex for async operations across await points
    runtime: Option<Arc<TokioMutex<StreamRuntime>>>,

    /// Granted permissions (e.g., "camera", "display")
    /// Set via --allow-camera, --allow-display CLI flags
    permissions: Arc<HashSet<String>>,

    /// Server name for MCP identification
    name: String,

    /// Server version
    version: String,
}

impl McpServer {
    /// Create a new MCP server in discovery mode (registry only)
    ///
    /// AI agents can discover available processor types but cannot control a running runtime.
    ///
    /// # Arguments
    /// * `registry` - Shared processor registry
    pub fn new(registry: Arc<Mutex<ProcessorRegistry>>) -> Self {
        Self {
            registry,
            runtime: None,
            permissions: Arc::new(HashSet::new()),
            name: "streamlib-mcp".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
        }
    }

    /// Create a new MCP server in application mode (registry + runtime)
    ///
    /// AI agents can both discover processor types AND control the running runtime
    /// (add/remove processors, list connections, etc.).
    ///
    /// # Arguments
    /// * `registry` - Shared processor registry (for type discovery)
    /// * `runtime` - Shared runtime (for live system control)
    pub fn with_runtime(
        registry: Arc<Mutex<ProcessorRegistry>>,
        runtime: Arc<TokioMutex<StreamRuntime>>,
    ) -> Self {
        Self {
            registry,
            runtime: Some(runtime),
            permissions: Arc::new(HashSet::new()),
            name: "streamlib-mcp".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
        }
    }

    /// Set permissions (typically called after with_runtime)
    pub fn with_permissions(mut self, permissions: HashSet<String>) -> Self {
        self.permissions = Arc::new(permissions);
        self
    }

    /// Create with custom name and version (discovery mode)
    pub fn with_info(
        registry: Arc<Mutex<ProcessorRegistry>>,
        name: impl Into<String>,
        version: impl Into<String>,
    ) -> Self {
        Self {
            registry,
            runtime: None,
            permissions: Arc::new(HashSet::new()),
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

        // Create handler with registry, optional runtime, and permissions
        let handler = StreamlibMcpHandler {
            registry: Arc::clone(&self.registry),
            runtime: self.runtime.as_ref().map(Arc::clone),
            permissions: Arc::clone(&self.permissions),
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
    /// Uses streamable HTTP transport.
    ///
    /// If the requested port is in use, automatically tries the next available port.
    ///
    /// # Arguments
    /// * `bind_addr` - Address to bind to (e.g., "127.0.0.1:3050")
    pub async fn run_http(&self, bind_addr: &str) -> Result<()> {
        // Parse the bind address to extract host and port
        let (host, requested_port) = Self::parse_bind_addr(bind_addr)?;

        // Find an available port starting from the requested port
        let (actual_host, actual_port) = Self::find_available_port(&host, requested_port).await?;
        let actual_bind_addr = format!("{}:{}", actual_host, actual_port);

        if actual_port != requested_port {
            tracing::warn!(
                "Requested port {} was in use, using port {} instead",
                requested_port,
                actual_port
            );
        }

        tracing::info!("Starting MCP server on HTTP at {}", actual_bind_addr);

        // Create factory for MCP services
        let registry = Arc::clone(&self.registry);
        let runtime = self.runtime.as_ref().map(Arc::clone);
        let permissions = Arc::clone(&self.permissions);
        let name = self.name.clone();
        let version = self.version.clone();

        let service_factory = move || {
            Ok(StreamlibMcpHandler {
                registry: Arc::clone(&registry),
                runtime: runtime.as_ref().map(Arc::clone),
                permissions: Arc::clone(&permissions),
                name: name.clone(),
                version: version.clone(),
            })
        };

        // Create StreamableHttpService with local session manager
        let session_manager = Arc::new(LocalSessionManager::default());
        let http_service = StreamableHttpService::new(
            service_factory,
            session_manager,
            StreamableHttpServerConfig::default(),
        );

        // Create Axum router with the MCP service
        // Note: nest_service at root is not supported, use fallback_service
        let app = axum::Router::new()
            .nest_service("/mcp", http_service.clone())
            .fallback_service(http_service);

        // Create TCP listener
        let listener = tokio::net::TcpListener::bind(&actual_bind_addr)
            .await
            .map_err(|e| McpError::Protocol(format!("Failed to bind to {}: {}", actual_bind_addr, e)))?;

        tracing::info!("MCP server running on HTTP at {}", actual_bind_addr);
        tracing::info!("Access the MCP server at http://{}/mcp", actual_bind_addr);

        // Serve the application
        axum::serve(listener, app)
            .await
            .map_err(|e| McpError::Protocol(format!("Server error: {}", e)))?;

        Ok(())
    }

    /// Parse bind address into host and port
    fn parse_bind_addr(bind_addr: &str) -> Result<(String, u16)> {
        let parts: Vec<&str> = bind_addr.rsplitn(2, ':').collect();
        if parts.len() != 2 {
            return Err(McpError::Configuration(
                format!("Invalid bind address: {}. Expected format: host:port", bind_addr)
            ));
        }

        let port = parts[0].parse::<u16>()
            .map_err(|_| McpError::Configuration(
                format!("Invalid port number: {}", parts[0])
            ))?;

        Ok((parts[1].to_string(), port))
    }

    /// Find an available port starting from the requested port
    ///
    /// Tries up to 100 consecutive ports before giving up
    async fn find_available_port(host: &str, start_port: u16) -> Result<(String, u16)> {
        const MAX_ATTEMPTS: u16 = 100;

        for offset in 0..MAX_ATTEMPTS {
            let port = start_port.saturating_add(offset);
            let addr = format!("{}:{}", host, port);

            // Try to bind to check if port is available
            match tokio::net::TcpListener::bind(&addr).await {
                Ok(_) => {
                    // Port is available
                    return Ok((host.to_string(), port));
                }
                Err(_) => {
                    // Port in use, try next
                    continue;
                }
            }
        }

        Err(McpError::Configuration(
            format!(
                "Could not find available port in range {}-{}",
                start_port,
                start_port.saturating_add(MAX_ATTEMPTS - 1)
            )
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
    runtime: Option<Arc<TokioMutex<StreamRuntime>>>,
    permissions: Arc<HashSet<String>>,
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
                        uri: r.uri,
                        name: r.name,
                        description: Some(r.description),
                        mime_type: Some(r.mime_type),
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
                contents: vec![ResourceContents::TextResourceContents {
                    uri: content.uri,
                    mime_type: Some(content.mime_type),
                    text: content.text,
                    meta: None,
                }],
            })
        }
    }

    // Implement tool listing
    async fn list_tools(
        &self,
        _params: Option<PaginatedRequestParam>,
        _ctx: RequestContext<RoleServer>,
    ) -> std::result::Result<ListToolsResult, RmcpError> {
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

    // Implement tool calling
    async fn call_tool(
        &self,
        params: CallToolRequestParam,
        _ctx: RequestContext<RoleServer>,
    ) -> std::result::Result<CallToolResult, RmcpError> {
        tracing::info!("MCP: call_tool called: {} with args: {:?}", params.name, params.arguments);

        // Convert arguments from Map to Value
        let arguments = serde_json::Value::Object(params.arguments.unwrap_or_default());

        // Pass registry, runtime, and permissions to tool execution
        let result = tools::execute_tool(
            &params.name,
            arguments,
            self.registry.clone(),
            self.runtime.as_ref().map(Arc::clone),
            self.permissions.clone()
        ).await
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::ProcessorRegistry;

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
