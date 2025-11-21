use super::{McpError, Result};
use crate::core::{ProcessorRegistry, StreamRuntime};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use std::collections::HashSet;
use std::sync::Arc;

#[derive(Debug, Clone, Serialize)]
pub struct Tool {
    pub name: String,

    pub description: String,

    #[serde(rename = "inputSchema")]
    pub input_schema: JsonValue,
}

#[derive(Debug, Clone, Serialize)]
pub struct ToolResult {
    pub success: bool,

    pub message: String,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<JsonValue>,
}

pub fn list_tools() -> Vec<Tool> {
    vec![
        Tool {
            name: "list_supported_languages".to_string(),
            description: "List programming languages supported for dynamic processor creation. Use this to discover what languages you can write processors in.".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {}
            }),
        },
        Tool {
            name: "list_packages".to_string(),
            description: "List packages currently installed and available for a specific language. Returns package names with versions.".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "language": {
                        "type": "string",
                        "description": "Language to list packages for (e.g., 'python', 'typescript')",
                        "enum": ["python"]
                    }
                },
                "required": ["language"]
            }),
        },
        Tool {
            name: "request_package".to_string(),
            description: "Request installation of a package for a specific language. The runtime will evaluate the request based on security policy (allowlist, auto-approve, require-approval, etc.) and may install the package if approved.".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "language": {
                        "type": "string",
                        "description": "Language for the package (e.g., 'python', 'typescript')",
                        "enum": ["python"]
                    },
                    "package": {
                        "type": "string",
                        "description": "Package name (e.g., 'scikit-learn', 'pillow'). Optionally include version: 'torch==2.0.0'"
                    },
                    "reason": {
                        "type": "string",
                        "description": "Brief explanation of why this package is needed (helps with approval decision)"
                    }
                },
                "required": ["language", "package"]
            }),
        },
        Tool {
            name: "get_package_status".to_string(),
            description: "Check the installation status of a package for a specific language. Returns whether it's installed, pending approval, or denied.".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "language": {
                        "type": "string",
                        "description": "Language for the package (e.g., 'python', 'typescript')",
                        "enum": ["python"]
                    },
                    "package": {
                        "type": "string",
                        "description": "Package name to check"
                    }
                },
                "required": ["language", "package"]
            }),
        },
        Tool {
            name: "add_processor".to_string(),
            description: "Add a processor to the runtime. For pre-compiled processors, provide just the name. For dynamic processors, provide code in a supported language (use list_supported_languages to see options).".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "description": "Name of the processor (for registry lookup or to identify dynamic processor)"
                    },
                    "language": {
                        "type": "string",
                        "description": "Programming language for dynamic processor (e.g., 'python', 'rust'). Omit for pre-compiled processors.",
                        "enum": ["python", "rust"]
                    },
                    "code": {
                        "type": "string",
                        "description": "Source code for dynamic processor. Required if language is specified."
                    }
                },
                "required": ["name"]
            }),
        },
        Tool {
            name: "remove_processor".to_string(),
            description: "Remove a processor from the runtime by name.".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "description": "Name of the processor to remove"
                    }
                },
                "required": ["name"]
            }),
        },
        Tool {
            name: "connect_processors".to_string(),
            description: "Connect two processors by linking an output port to an input port. Data will flow from source to destination. Ports are compatible if their schemas match (e.g., both use VideoFrame schema).".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "source": {
                        "type": "string",
                        "description": "Source OUTPUT port in format 'processor_id.port_name' (e.g., 'processor_0.video'). Use list_processors to get processor IDs. The source must be an output port."
                    },
                    "destination": {
                        "type": "string",
                        "description": "Destination INPUT port in format 'processor_id.port_name' (e.g., 'processor_1.video'). Use list_processors to get processor IDs. The destination must be an input port."
                    }
                },
                "required": ["source", "destination"]
            }),
        },
        Tool {
            name: "list_processors".to_string(),
            description: "List all processors currently in the runtime (not just available in registry). Shows processor IDs, names, and status (Pending, Running, Stopping, Stopped).".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {}
            }),
        },
        Tool {
            name: "list_connections".to_string(),
            description: "List all connections between processors in the runtime. Shows source ports, destination ports, and connection IDs.".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {}
            }),
        },
    ]
}

#[derive(Debug, Deserialize)]
pub struct ListPackagesArgs {
    pub language: String,
}

#[derive(Debug, Deserialize)]
pub struct RequestPackageArgs {
    pub language: String,
    pub package: String,
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct GetPackageStatusArgs {
    pub language: String,
    pub package: String,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)] // Fields reserved for future processor creation API
pub struct AddProcessorArgs {
    pub name: String,
    #[serde(default)]
    pub language: Option<String>,
    #[serde(default)]
    pub code: Option<String>,
    #[serde(default)]
    pub config: Option<JsonValue>,
}

#[derive(Debug, Deserialize)]
pub struct RemoveProcessorArgs {
    pub name: String,
}

#[derive(Debug, Deserialize)]
pub struct ConnectProcessorsArgs {
    pub source: String,
    pub destination: String,
}

pub async fn execute_tool(
    tool_name: &str,
    arguments: JsonValue,
    _registry: Arc<Mutex<ProcessorRegistry>>,
    runtime: Option<Arc<Mutex<StreamRuntime>>>,
    _permissions: Arc<HashSet<String>>,
) -> Result<ToolResult> {
    match tool_name {
        "list_supported_languages" => Ok(ToolResult {
            success: true,
            message: "Supported languages for dynamic processor creation".to_string(),
            data: Some(serde_json::json!({
                "languages": [
                    {
                        "name": "python",
                        "version": "3.11+",
                        "status": "ready",
                        "description": "Python via PyO3 embedded interpreter - create custom processors with @processor decorator"
                    }
                ]
            })),
        }),

        "list_packages" => {
            let args: ListPackagesArgs =
                serde_json::from_value(arguments).map_err(|e| McpError::InvalidArguments {
                    tool: tool_name.to_string(),
                    message: e.to_string(),
                })?;

            if args.language != "python" {
                return Err(McpError::InvalidArguments {
                    tool: tool_name.to_string(),
                    message: format!(
                        "Unsupported language: {}. Currently only 'python' is supported.",
                        args.language
                    ),
                });
            }

            // TODO: Query actual Python environment via PyO3 to list installed packages
            Ok(ToolResult {
                success: true,
                message: format!("Packages available for {}", args.language),
                data: Some(serde_json::json!({
                    "language": args.language,
                    "packages": [],
                    "note": "Package listing not yet implemented. Python processors work with standard library by default."
                })),
            })
        }

        "request_package" => {
            let args: RequestPackageArgs =
                serde_json::from_value(arguments).map_err(|e| McpError::InvalidArguments {
                    tool: tool_name.to_string(),
                    message: e.to_string(),
                })?;

            if args.language != "python" {
                return Err(McpError::InvalidArguments {
                    tool: tool_name.to_string(),
                    message: format!(
                        "Unsupported language: {}. Currently only 'python' is supported.",
                        args.language
                    ),
                });
            }

            // TODO: Implement package installation with security policy evaluation
            Ok(ToolResult {
                success: true,
                message: format!(
                    "{} package '{}' request received{}",
                    args.language,
                    args.package,
                    args.reason
                        .as_ref()
                        .map(|r| format!(" (reason: {})", r))
                        .unwrap_or_default()
                ),
                data: Some(serde_json::json!({
                    "language": args.language,
                    "package": args.package,
                    "status": "not_implemented",
                    "message": "Package installation not yet implemented. Use Python standard library for now."
                })),
            })
        }

        "get_package_status" => {
            let args: GetPackageStatusArgs =
                serde_json::from_value(arguments).map_err(|e| McpError::InvalidArguments {
                    tool: tool_name.to_string(),
                    message: e.to_string(),
                })?;

            if args.language != "python" {
                return Err(McpError::InvalidArguments {
                    tool: tool_name.to_string(),
                    message: format!(
                        "Unsupported language: {}. Currently only 'python' is supported.",
                        args.language
                    ),
                });
            }

            // TODO: Implement package status checking
            Ok(ToolResult {
                success: true,
                message: format!("Status for {} package '{}'", args.language, args.package),
                data: Some(serde_json::json!({
                    "language": args.language,
                    "package": args.package,
                    "status": "unknown",
                    "message": "Package status check not yet implemented. Python processors work with standard library by default."
                })),
            })
        }

        "add_processor" => {
            tracing::info!("add_processor tool called");
            let args: AddProcessorArgs =
                serde_json::from_value(arguments).map_err(|e| McpError::InvalidArguments {
                    tool: tool_name.to_string(),
                    message: e.to_string(),
                })?;

            tracing::info!("add_processor args parsed: {:?}", args);

            let _runtime = runtime.ok_or_else(|| {
                tracing::error!("add_processor called without runtime");
                McpError::Runtime(
                    "add_processor requires runtime access. MCP server is in discovery mode (registry only). \
                     Use McpServer::with_runtime() to enable application-level control.".to_string()
                )
            })?;

            tracing::info!("Runtime available, checking permissions...");

            if let (Some(_language), Some(_code)) = (&args.language, &args.code) {
                // Python processor creation from code is not yet implemented
                return Err(McpError::Runtime(
                    "Python processors from code are not yet implemented. Use the processor registry to add built-in processors.".to_string()
                ));
            }

            Err(McpError::InvalidArguments {
                tool: tool_name.to_string(),
                message: "add_processor requires Python code. Example:\n\
                    language: \"python\"\n\
                    code: \"\
                    @camera_processor(device_id='0x1424001bcf2284')\n\
                    def camera():\n\
                        pass\"\n\n\
                    Check the processor registry for available decorators (@camera_processor, @display_processor, @processor).".to_string(),
            })
        }

        "remove_processor" => {
            let args: RemoveProcessorArgs =
                serde_json::from_value(arguments).map_err(|e| McpError::InvalidArguments {
                    tool: tool_name.to_string(),
                    message: e.to_string(),
                })?;

            let runtime = runtime.ok_or_else(|| {
                McpError::Runtime(
                    "remove_processor requires runtime access. MCP server is in discovery mode (registry only). \
                     Use McpServer::with_runtime() to enable application-level control.".to_string()
                )
            })?;

            let runtime_clone = Arc::clone(&runtime);
            let name = args.name.clone();
            let result = tokio::task::spawn_blocking(move || {
                let mut rt = runtime_clone.lock();
                rt.remove_processor(&name)
            })
            .await
            .map_err(|e| McpError::Runtime(format!("Failed to spawn blocking task: {}", e)))?;

            match result {
                Ok(_) => Ok(ToolResult {
                    success: true,
                    message: format!("Successfully removed processor '{}'", args.name),
                    data: Some(serde_json::json!({
                        "processor_id": args.name
                    })),
                }),
                Err(e) => Ok(ToolResult {
                    success: false,
                    message: format!("Failed to remove processor '{}': {}", args.name, e),
                    data: None,
                }),
            }
        }

        "connect_processors" => {
            let args: ConnectProcessorsArgs =
                serde_json::from_value(arguments).map_err(|e| McpError::InvalidArguments {
                    tool: tool_name.to_string(),
                    message: e.to_string(),
                })?;

            let runtime = runtime.ok_or_else(|| {
                McpError::Runtime(
                    "connect_processors requires runtime access. MCP server is in discovery mode (registry only). \
                     Use McpServer::with_runtime() to enable application-level control.".to_string()
                )
            })?;

            let runtime_clone = Arc::clone(&runtime);
            let source = args.source.clone();
            let destination = args.destination.clone();
            let result = tokio::task::spawn_blocking(move || {
                let mut rt = runtime_clone.lock();
                rt.connect_at_runtime(&source, &destination)
            })
            .await
            .map_err(|e| McpError::Runtime(format!("Failed to spawn blocking task: {}", e)))?;

            match result {
                Ok(connection_id) => Ok(ToolResult {
                    success: true,
                    message: format!(
                        "Successfully connected {} → {}",
                        args.source, args.destination
                    ),
                    data: Some(serde_json::json!({
                        "connection_id": connection_id,
                        "source": args.source,
                        "destination": args.destination
                    })),
                }),
                Err(e) => Ok(ToolResult {
                    success: false,
                    message: format!(
                        "Failed to connect {} → {}: {}",
                        args.source, args.destination, e
                    ),
                    data: None,
                }),
            }
        }

        "list_processors" => {
            let runtime = runtime.ok_or_else(|| {
                McpError::Runtime(
                    "list_processors requires runtime access. MCP server is in discovery mode (registry only). \
                     Use McpServer::with_runtime() to enable application-level control.".to_string()
                )
            })?;

            let runtime_clone = Arc::clone(&runtime);
            let processors = tokio::task::spawn_blocking(move || {
                let rt = runtime_clone.lock();
                let processors_map = rt.processors.lock();

                processors_map
                    .iter()
                    .map(|(id, handle)| {
                        let status = *handle.status.lock();
                        serde_json::json!({
                            "id": id,
                            "name": handle.name,
                            "status": format!("{:?}", status)
                        })
                    })
                    .collect::<Vec<_>>()
            })
            .await
            .map_err(|e| McpError::Runtime(format!("Failed to spawn blocking task: {}", e)))?;

            Ok(ToolResult {
                success: true,
                message: format!("Found {} processor(s) in runtime", processors.len()),
                data: Some(serde_json::json!({
                    "processors": processors,
                    "total_count": processors.len()
                })),
            })
        }

        "list_connections" => {
            let runtime = runtime.ok_or_else(|| {
                McpError::Runtime(
                    "list_connections requires runtime access. MCP server is in discovery mode (registry only). \
                     Use McpServer::with_runtime() to enable application-level control.".to_string()
                )
            })?;

            let runtime_clone = Arc::clone(&runtime);
            let connections = tokio::task::spawn_blocking(move || {
                let rt = runtime_clone.lock();
                let connections_map = rt.connections.lock();

                connections_map
                    .iter()
                    .map(|(id, conn)| {
                        serde_json::json!({
                            "id": id,
                            "from_port": conn.from_port,
                            "to_port": conn.to_port,
                            "created_at": format!("{:?}", conn.created_at)
                        })
                    })
                    .collect::<Vec<_>>()
            })
            .await
            .map_err(|e| McpError::Runtime(format!("Failed to spawn blocking task: {}", e)))?;

            Ok(ToolResult {
                success: true,
                message: format!("Found {} connection(s) in runtime", connections.len()),
                data: Some(serde_json::json!({
                    "connections": connections,
                    "total_count": connections.len()
                })),
            })
        }

        _ => Err(McpError::ToolNotFound(tool_name.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_list_tools() {
        let tools = list_tools();
        assert_eq!(tools.len(), 9);

        let tool_names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
        assert!(tool_names.contains(&"list_supported_languages"));
        assert!(tool_names.contains(&"list_packages"));
        assert!(tool_names.contains(&"request_package"));
        assert!(tool_names.contains(&"get_package_status"));
        assert!(tool_names.contains(&"add_processor"));
        assert!(tool_names.contains(&"remove_processor"));
        assert!(tool_names.contains(&"connect_processors"));
        assert!(tool_names.contains(&"list_processors"));
        assert!(tool_names.contains(&"list_connections"));
    }

    #[test]
    fn test_tool_has_schema() {
        let tools = list_tools();
        let add_processor = tools.iter().find(|t| t.name == "add_processor").unwrap();

        assert!(add_processor.input_schema["properties"]["name"].is_object());
        assert_eq!(
            add_processor.input_schema["required"],
            serde_json::json!(["name"])
        );
    }

    #[tokio::test]
    async fn test_execute_unknown_tool() {
        use crate::core::ProcessorRegistry;
        let registry = Arc::new(Mutex::new(ProcessorRegistry::new()));
        let permissions = Arc::new(HashSet::new());
        let result = execute_tool(
            "unknown_tool",
            serde_json::json!({}),
            registry,
            None,
            permissions,
        )
        .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_execute_add_processor_placeholder() {
        use crate::core::ProcessorRegistry;
        let registry = Arc::new(Mutex::new(ProcessorRegistry::new()));
        let permissions = Arc::new(HashSet::new());
        let result = execute_tool(
            "add_processor",
            serde_json::json!({
                "name": "CameraProcessor"
            }),
            registry,
            None, // Discovery mode
            permissions,
        )
        .await;

        assert!(result.is_err()); // Should fail because runtime is None
    }

    #[tokio::test]
    async fn test_execute_invalid_arguments() {
        use crate::core::ProcessorRegistry;
        let registry = Arc::new(Mutex::new(ProcessorRegistry::new()));
        let permissions = Arc::new(HashSet::new());
        let result = execute_tool(
            "add_processor",
            serde_json::json!({
                "invalid": "arguments"
            }),
            registry,
            None, // Discovery mode
            permissions,
        )
        .await;

        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_list_processors_requires_runtime() {
        use crate::core::ProcessorRegistry;
        let registry = Arc::new(Mutex::new(ProcessorRegistry::new()));
        let permissions = Arc::new(HashSet::new());
        let result = execute_tool(
            "list_processors",
            serde_json::json!({}),
            registry,
            None,
            permissions,
        )
        .await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, McpError::Runtime(_)));
    }

    #[tokio::test]
    async fn test_list_connections_requires_runtime() {
        use crate::core::ProcessorRegistry;
        let registry = Arc::new(Mutex::new(ProcessorRegistry::new()));
        let permissions = Arc::new(HashSet::new());
        let result = execute_tool(
            "list_connections",
            serde_json::json!({}),
            registry,
            None,
            permissions,
        )
        .await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, McpError::Runtime(_)));
    }
}
