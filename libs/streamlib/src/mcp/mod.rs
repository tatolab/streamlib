
mod error;
mod resources;
mod tools;
mod package_manager;

pub use error::{McpError, Result};
pub use package_manager::{PackageManager, PackageInfo, PackageStatus, ApprovalPolicy};

use crate::core::{ProcessorRegistry, StreamRuntime};
use std::sync::Arc;
use parking_lot::Mutex;
use std::future::Future;
use std::collections::HashSet;

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

pub struct McpServer {
    registry: Arc<Mutex<ProcessorRegistry>>,

    runtime: Option<Arc<Mutex<StreamRuntime>>>,

    permissions: Arc<HashSet<String>>,

    name: String,

    version: String,
}

impl McpServer {
    pub fn new(registry: Arc<Mutex<ProcessorRegistry>>) -> Self {
        Self {
            registry,
            runtime: None,
            permissions: Arc::new(HashSet::new()),
            name: "streamlib-mcp".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
        }
    }

    pub fn with_runtime(
        registry: Arc<Mutex<ProcessorRegistry>>,
        runtime: Arc<Mutex<StreamRuntime>>,
    ) -> Self {
        Self {
            registry,
            runtime: Some(runtime),
            permissions: Arc::new(HashSet::new()),
            name: "streamlib-mcp".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
        }
    }

    pub fn with_permissions(mut self, permissions: HashSet<String>) -> Self {
        self.permissions = Arc::new(permissions);
        self
    }

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

    pub async fn run_stdio(&self) -> Result<()> {
        tracing::info!("Starting MCP server on stdio");

        let handler = StreamlibMcpHandler {
            registry: Arc::clone(&self.registry),
            runtime: self.runtime.as_ref().map(Arc::clone),
            permissions: Arc::clone(&self.permissions),
            name: self.name.clone(),
            version: self.version.clone(),
        };

        let service = handler.serve(stdio()).await
            .map_err(|e| McpError::Protocol(format!("Failed to start stdio server: {}", e)))?;

        tracing::info!("MCP server running on stdio, awaiting requests");

        service.waiting().await
            .map_err(|e| McpError::Protocol(format!("Server error: {}", e)))?;

        Ok(())
    }

    pub async fn run_http(&self, bind_addr: &str) -> Result<()> {
        let (host, requested_port) = Self::parse_bind_addr(bind_addr)?;

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

        let session_manager = Arc::new(LocalSessionManager::default());
        let http_service = StreamableHttpService::new(
            service_factory,
            session_manager,
            StreamableHttpServerConfig::default(),
        );

        let app = axum::Router::new()
            .nest_service("/mcp", http_service.clone())
            .fallback_service(http_service);

        let listener = tokio::net::TcpListener::bind(&actual_bind_addr)
            .await
            .map_err(|e| McpError::Protocol(format!("Failed to bind to {}: {}", actual_bind_addr, e)))?;

        tracing::info!("MCP server running on HTTP at {}", actual_bind_addr);
        tracing::info!("Access the MCP server at http://{}/mcp", actual_bind_addr);

        axum::serve(listener, app)
            .await
            .map_err(|e| McpError::Protocol(format!("Server error: {}", e)))?;

        Ok(())
    }

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

    async fn find_available_port(host: &str, start_port: u16) -> Result<(String, u16)> {
        const MAX_ATTEMPTS: u16 = 100;

        for offset in 0..MAX_ATTEMPTS {
            let port = start_port.saturating_add(offset);
            let addr = format!("{}:{}", host, port);

            match tokio::net::TcpListener::bind(&addr).await {
                Ok(_) => {
                    return Ok((host.to_string(), port));
                }
                Err(_) => {
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

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn version(&self) -> &str {
        &self.version
    }

    pub fn registry(&self) -> Arc<Mutex<ProcessorRegistry>> {
        Arc::clone(&self.registry)
    }
}

#[derive(Clone)]
struct StreamlibMcpHandler {
    registry: Arc<Mutex<ProcessorRegistry>>,
    runtime: Option<Arc<Mutex<StreamRuntime>>>,
    permissions: Arc<HashSet<String>>,
    name: String,
    version: String,
}

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

    async fn list_tools(
        &self,
        _params: Option<PaginatedRequestParam>,
        _ctx: RequestContext<RoleServer>,
    ) -> std::result::Result<ListToolsResult, RmcpError> {
        tracing::debug!("MCP: list_tools called");

        let tools = tools::list_tools();

        let tools: Vec<Tool> = tools
            .into_iter()
            .map(|t| {
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

    async fn call_tool(
        &self,
        params: CallToolRequestParam,
        _ctx: RequestContext<RoleServer>,
    ) -> std::result::Result<CallToolResult, RmcpError> {
        tracing::info!("MCP: call_tool called: {} with args: {:?}", params.name, params.arguments);

        let arguments = serde_json::Value::Object(params.arguments.unwrap_or_default());

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
