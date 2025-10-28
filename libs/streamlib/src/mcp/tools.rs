//! MCP Tools - Runtime Actions
//!
//! Tools expose actions that AI agents can invoke to modify the runtime.
//! Examples: add_processor, remove_processor, connect_processors

use super::{McpError, Result};
use crate::core::{StreamRuntime, ProcessorRegistry};
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use std::sync::{Arc, Mutex};
use std::collections::HashSet;
use tokio::sync::Mutex as TokioMutex;

/// MCP Tool definition
#[derive(Debug, Clone, Serialize)]
pub struct Tool {
    /// Tool name (e.g., "add_processor")
    pub name: String,

    /// Tool description (for AI agent)
    pub description: String,

    /// JSON Schema for input parameters
    #[serde(rename = "inputSchema")]
    pub input_schema: JsonValue,
}

/// Tool execution result
#[derive(Debug, Clone, Serialize)]
pub struct ToolResult {
    /// Whether the tool succeeded
    pub success: bool,

    /// Result message
    pub message: String,

    /// Optional result data
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<JsonValue>,
}

/// List all available tools
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

/// Arguments for list_packages tool
#[derive(Debug, Deserialize)]
pub struct ListPackagesArgs {
    pub language: String,
}

/// Arguments for request_package tool
#[derive(Debug, Deserialize)]
pub struct RequestPackageArgs {
    pub language: String,
    pub package: String,
    #[serde(default)]
    pub reason: Option<String>,
}

/// Arguments for get_package_status tool
#[derive(Debug, Deserialize)]
pub struct GetPackageStatusArgs {
    pub language: String,
    pub package: String,
}

/// Arguments for add_processor tool
#[derive(Debug, Deserialize)]
pub struct AddProcessorArgs {
    pub name: String,
    #[serde(default)]
    pub language: Option<String>,
    #[serde(default)]
    pub code: Option<String>,
    #[serde(default)]
    pub config: Option<JsonValue>,
}

/// Arguments for remove_processor tool
#[derive(Debug, Deserialize)]
pub struct RemoveProcessorArgs {
    pub name: String,
}

/// Arguments for connect_processors tool
#[derive(Debug, Deserialize)]
pub struct ConnectProcessorsArgs {
    pub source: String,
    pub destination: String,
}

/// Execute a tool
///
/// # Arguments
/// * `tool_name` - Name of the tool to execute
/// * `arguments` - JSON arguments for the tool
/// * `registry` - Processor registry for creating processor instances
/// * `runtime` - Optional runtime for application-level tools (add/remove processors, list connections, etc.)
/// * `permissions` - Granted permissions (e.g., "camera", "display") from CLI flags
///
/// If runtime is None, only discovery-level tools are available (list available processor types).
/// If runtime is Some, full application control is enabled (modify running system).
pub async fn execute_tool(
    tool_name: &str,
    arguments: JsonValue,
    registry: Arc<Mutex<ProcessorRegistry>>,
    runtime: Option<Arc<TokioMutex<StreamRuntime>>>,
    permissions: Arc<HashSet<String>>,
) -> Result<ToolResult> {
    match tool_name {
        "list_supported_languages" => {
            // Return list of languages the runtime supports for dynamic processors
            // Only python is currently supported (PyO3 integration in progress)
            Ok(ToolResult {
                success: true,
                message: "Supported languages for dynamic processor creation".to_string(),
                data: Some(serde_json::json!({
                    "languages": [
                        {
                            "name": "python",
                            "version": "3.11+",
                            "status": "in_progress",
                            "description": "Python via PyO3 embedded interpreter (streamlib-python crate in progress)"
                        }
                    ]
                })),
            })
        }

        "list_packages" => {
            let args: ListPackagesArgs = serde_json::from_value(arguments)
                .map_err(|e| McpError::InvalidArguments {
                    tool: tool_name.to_string(),
                    message: e.to_string(),
                })?;

            // Validate language
            if args.language != "python" {
                return Err(McpError::InvalidArguments {
                    tool: tool_name.to_string(),
                    message: format!("Unsupported language: {}. Currently only 'python' is supported.", args.language),
                });
            }

            // TODO: Query actual Python environment via PyO3 once integrated
            // For now, return empty list since Python runtime isn't initialized
            Ok(ToolResult {
                success: true,
                message: format!("Packages available for {}", args.language),
                data: Some(serde_json::json!({
                    "language": args.language,
                    "packages": [],
                    "note": "Python runtime not yet initialized. Once streamlib-python is integrated, this will return actual installed packages."
                })),
            })
        }

        "request_package" => {
            let args: RequestPackageArgs = serde_json::from_value(arguments)
                .map_err(|e| McpError::InvalidArguments {
                    tool: tool_name.to_string(),
                    message: e.to_string(),
                })?;

            // Validate language
            if args.language != "python" {
                return Err(McpError::InvalidArguments {
                    tool: tool_name.to_string(),
                    message: format!("Unsupported language: {}. Currently only 'python' is supported.", args.language),
                });
            }

            // TODO: Implement actual package request handling
            // This will evaluate against security policy and potentially install
            Ok(ToolResult {
                success: true,
                message: format!(
                    "{} package '{}' request received{}",
                    args.language,
                    args.package,
                    args.reason.as_ref()
                        .map(|r| format!(" (reason: {})", r))
                        .unwrap_or_default()
                ),
                data: Some(serde_json::json!({
                    "language": args.language,
                    "package": args.package,
                    "status": "pending_approval",
                    "message": "Package installation requires approval (placeholder - will be functional once streamlib-python is integrated)"
                })),
            })
        }

        "get_package_status" => {
            let args: GetPackageStatusArgs = serde_json::from_value(arguments)
                .map_err(|e| McpError::InvalidArguments {
                    tool: tool_name.to_string(),
                    message: e.to_string(),
                })?;

            // Validate language
            if args.language != "python" {
                return Err(McpError::InvalidArguments {
                    tool: tool_name.to_string(),
                    message: format!("Unsupported language: {}. Currently only 'python' is supported.", args.language),
                });
            }

            // TODO: Implement actual package status check
            Ok(ToolResult {
                success: true,
                message: format!("Status for {} package '{}'", args.language, args.package),
                data: Some(serde_json::json!({
                    "language": args.language,
                    "package": args.package,
                    "status": "not_installed",
                    "message": "Package status check not yet implemented (will be functional once streamlib-python is integrated)"
                })),
            })
        }

        "add_processor" => {
            tracing::info!("add_processor tool called");
            let args: AddProcessorArgs = serde_json::from_value(arguments)
                .map_err(|e| McpError::InvalidArguments {
                    tool: tool_name.to_string(),
                    message: e.to_string(),
                })?;

            tracing::info!("add_processor args parsed: {:?}", args);

            // Check if runtime is available
            let runtime = runtime.ok_or_else(|| {
                tracing::error!("add_processor called without runtime");
                McpError::Runtime(
                    "add_processor requires runtime access. MCP server is in discovery mode (registry only). \
                     Use McpServer::with_runtime() to enable application-level control.".to_string()
                )
            })?;

            tracing::info!("Runtime available, checking permissions...");

            // Handle dynamic Python processors
            if let (Some(language), Some(code)) = (&args.language, &args.code) {
                if language != "python" {
                    return Err(McpError::InvalidArguments {
                        tool: tool_name.to_string(),
                        message: format!("Only 'python' language is currently supported, got '{}'", language),
                    });
                }

                #[cfg(not(feature = "python-embed"))]
                {
                    return Err(McpError::Runtime(
                        "Python processors require the 'python-embed' feature to be enabled. Rebuild MCP server with --features python-embed.".to_string()
                    ));
                }

                #[cfg(feature = "python-embed")]
                {
                    use pyo3::prelude::*;
                    use crate::python::PythonProcessor;

                    // Execute Python code and extract ProcessorProxy
                    let processor = Python::with_gil(|py| -> crate::Result<Box<dyn crate::StreamProcessor>> {
                        // Register streamlib module into sys.modules so imports work
                        // (only needed for python-embed mode, not extension-module mode)
                        let streamlib_module = pyo3::types::PyModule::new_bound(py, "streamlib")
                            .map_err(|e| crate::core::StreamError::Configuration(
                                format!("Failed to create streamlib module: {}", e)
                            ))?;

                        crate::python::register_python_module(&streamlib_module)
                            .map_err(|e| crate::core::StreamError::Configuration(
                                format!("Failed to register streamlib module: {}", e)
                            ))?;

                        // Add streamlib to sys.modules
                        py.import_bound("sys")
                            .map_err(|e| crate::core::StreamError::Configuration(
                                format!("Failed to import sys: {}", e)
                            ))?
                            .getattr("modules")
                            .map_err(|e| crate::core::StreamError::Configuration(
                                format!("Failed to get sys.modules: {}", e)
                            ))?
                            .set_item("streamlib", streamlib_module)
                            .map_err(|e| crate::core::StreamError::Configuration(
                                format!("Failed to add streamlib to sys.modules: {}", e)
                            ))?;

                        // Now execute the user's code exactly as provided (like a notebook cell)
                        let locals = pyo3::types::PyDict::new_bound(py);
                        py.run_bound(code, None, Some(&locals))
                            .map_err(|e| crate::core::StreamError::Configuration(
                                format!("Failed to execute Python code: {}", e)
                            ))?;

                        // Extract the ProcessorProxy from locals (find the decorated function)
                        let proxy = locals.values()
                            .iter()
                            .find(|v| {
                                // Check if this value has processor_name attribute (indicates ProcessorProxy)
                                v.hasattr("processor_name").unwrap_or(false)
                            })
                            .ok_or_else(|| crate::core::StreamError::Configuration(
                                "Python code did not define a processor (no decorated function found)".to_string()
                            ))?;

                        // Extract ProcessorProxy metadata
                        let processor_name: String = proxy.getattr("processor_name")
                            .map_err(|e| crate::core::StreamError::Configuration(
                                format!("Invalid processor: {}", e)
                            ))?
                            .extract()
                            .map_err(|e| crate::core::StreamError::Configuration(
                                format!("Invalid processor_name: {}", e)
                            ))?;

                        let processor_type: String = proxy.getattr("processor_type")
                            .map_err(|e| crate::core::StreamError::Configuration(
                                format!("Invalid processor: {}", e)
                            ))?
                            .extract()
                            .map_err(|e| crate::core::StreamError::Configuration(
                                format!("Invalid processor_type: {}", e)
                            ))?;

                        // For Python processors, extract the python_class
                        let python_class = proxy.getattr("python_class")
                            .ok()
                            .and_then(|c| if c.is_none() { None } else { Some(c.into()) });

                        if let Some(python_class) = python_class {
                            // Custom Python processor
                            let input_ports: Vec<String> = proxy.getattr("input_port_names")
                                .map_err(|e| crate::core::StreamError::Configuration(format!("Missing input_port_names: {}", e)))?
                                .extract()
                                .map_err(|e| crate::core::StreamError::Configuration(format!("Invalid input_port_names: {}", e)))?;
                            let output_ports: Vec<String> = proxy.getattr("output_port_names")
                                .map_err(|e| crate::core::StreamError::Configuration(format!("Missing output_port_names: {}", e)))?
                                .extract()
                                .map_err(|e| crate::core::StreamError::Configuration(format!("Invalid output_port_names: {}", e)))?;
                            let description: Option<String> = proxy.getattr("description").ok().and_then(|d| d.extract().ok());
                            let usage_context: Option<String> = proxy.getattr("usage_context").ok().and_then(|u| u.extract().ok());
                            let tags: Vec<String> = proxy.getattr("tags").ok().and_then(|t| t.extract().ok()).unwrap_or_default();

                            let py_processor = PythonProcessor::new(
                                python_class,
                                processor_name,
                                input_ports,
                                output_ports,
                                description,
                                usage_context,
                                tags,
                            )?;

                            Ok(Box::new(py_processor) as Box<dyn crate::StreamProcessor>)
                        } else {
                            // Pre-built processor (Camera, Display) - extract config and create Rust processor
                            let config_dict = proxy.getattr("config")
                                .ok()
                                .and_then(|c| if c.is_none() { None } else { Some(c) });

                            match processor_type.as_str() {
                                #[cfg(any(target_os = "macos", target_os = "ios"))]
                                "CameraProcessor" => {
                                    use crate::apple::main_thread::execute_on_main_thread;
                                    use crate::apple::processors::AppleCameraProcessor;

                                    let device_id = config_dict
                                        .and_then(|c| c.get_item("device_id").ok())
                                        .and_then(|d| d.extract::<String>().ok());

                                    execute_on_main_thread(move || {
                                        let p = if let Some(device_id) = device_id {
                                            AppleCameraProcessor::with_device_id(&device_id)?
                                        } else {
                                            AppleCameraProcessor::new()?
                                        };
                                        Ok(Box::new(p) as Box<dyn crate::StreamProcessor>)
                                    })
                                }

                                #[cfg(any(target_os = "macos", target_os = "ios"))]
                                "DisplayProcessor" => {
                                    use crate::apple::main_thread::execute_on_main_thread;
                                    use crate::apple::processors::AppleDisplayProcessor;

                                    execute_on_main_thread(|| {
                                        let p = AppleDisplayProcessor::new()?;
                                        Ok(Box::new(p) as Box<dyn crate::StreamProcessor>)
                                    })
                                }

                                _ => Err(crate::core::StreamError::Configuration(
                                    format!("Unknown pre-built processor type: {}", processor_type)
                                ))
                            }
                        }
                    }).map_err(|e| {
                        tracing::error!("Failed to create Python processor: {}", e);
                        McpError::Runtime(format!("Failed to create Python processor: {}", e))
                    })?;

                    // Add to runtime
                    let mut rt = runtime.lock().await;
                    match rt.add_processor_runtime(processor).await {
                        Ok(processor_id) => {
                            return Ok(ToolResult {
                                success: true,
                                message: format!("Successfully added Python processor"),
                                data: Some(serde_json::json!({
                                    "processor_id": processor_id,
                                })),
                            });
                        }
                        Err(e) => {
                            return Ok(ToolResult {
                                success: false,
                                message: format!("Failed to add Python processor: {}", e),
                                data: None,
                            });
                        }
                    }
                }
            }

            // If we get here, no language/code was provided
            // All processors must be added via Python code
            Err(McpError::InvalidArguments {
                tool: tool_name.to_string(),
                message: format!(
                    "add_processor requires Python code. Example:\n\
                    language: \"python\"\n\
                    code: \"\
                    @camera_processor(device_id='0x1424001bcf2284')\n\
                    def camera():\n\
                        pass\"\n\n\
                    Check the processor registry for available decorators (@camera_processor, @display_processor, @processor)."
                ),
            })
        }

        "remove_processor" => {
            let args: RemoveProcessorArgs = serde_json::from_value(arguments)
                .map_err(|e| McpError::InvalidArguments {
                    tool: tool_name.to_string(),
                    message: e.to_string(),
                })?;

            // Check if runtime is available
            let runtime = runtime.ok_or_else(|| {
                McpError::Runtime(
                    "remove_processor requires runtime access. MCP server is in discovery mode (registry only). \
                     Use McpServer::with_runtime() to enable application-level control.".to_string()
                )
            })?;

            // Call runtime's remove_processor method
            // Use tokio::sync::Mutex which is Send and can be held across await
            let mut rt = runtime.lock().await;
            match rt.remove_processor(&args.name).await {
                Ok(_) => {
                    Ok(ToolResult {
                        success: true,
                        message: format!("Successfully removed processor '{}'", args.name),
                        data: Some(serde_json::json!({
                            "processor_id": args.name
                        })),
                    })
                }
                Err(e) => {
                    Ok(ToolResult {
                        success: false,
                        message: format!("Failed to remove processor '{}': {}", args.name, e),
                        data: None,
                    })
                }
            }
        }

        "connect_processors" => {
            let args: ConnectProcessorsArgs = serde_json::from_value(arguments)
                .map_err(|e| McpError::InvalidArguments {
                    tool: tool_name.to_string(),
                    message: e.to_string(),
                })?;

            // Check if runtime is available
            let runtime = runtime.ok_or_else(|| {
                McpError::Runtime(
                    "connect_processors requires runtime access. MCP server is in discovery mode (registry only). \
                     Use McpServer::with_runtime() to enable application-level control.".to_string()
                )
            })?;

            // Connect processors at runtime (Phase 5)
            let mut rt = runtime.lock().await;
            match rt.connect_at_runtime(&args.source, &args.destination).await {
                Ok(connection_id) => {
                    Ok(ToolResult {
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
                    })
                }
                Err(e) => {
                    Ok(ToolResult {
                        success: false,
                        message: format!(
                            "Failed to connect {} → {}: {}",
                            args.source, args.destination, e
                        ),
                        data: None,
                    })
                }
            }
        }

        "list_processors" => {
            // Check if runtime is available
            let runtime = runtime.ok_or_else(|| {
                McpError::Runtime(
                    "list_processors requires runtime access. MCP server is in discovery mode (registry only). \
                     Use McpServer::with_runtime() to enable application-level control.".to_string()
                )
            })?;

            // Access processors registry and clone the data we need
            let processors = {
                let rt = runtime.lock().await;
                let processors_map = rt.processors.lock().unwrap();

                // Build processor list while holding the lock
                processors_map
                    .iter()
                    .map(|(id, handle)| {
                        let status = *handle.status.lock().unwrap();
                        serde_json::json!({
                            "id": id,
                            "name": handle.name,
                            "status": format!("{:?}", status)
                        })
                    })
                    .collect::<Vec<_>>()
            };

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
            // Check if runtime is available
            let runtime = runtime.ok_or_else(|| {
                McpError::Runtime(
                    "list_connections requires runtime access. MCP server is in discovery mode (registry only). \
                     Use McpServer::with_runtime() to enable application-level control.".to_string()
                )
            })?;

            // Access connections registry and clone the data we need
            let connections = {
                let rt = runtime.lock().await;
                let connections_map = rt.connections.lock().unwrap();

                // Build connections list while holding the lock
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
            };

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
        let result = execute_tool("unknown_tool", serde_json::json!({}), registry, None).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_execute_add_processor_placeholder() {
        use crate::core::ProcessorRegistry;
        let registry = Arc::new(Mutex::new(ProcessorRegistry::new()));
        let result = execute_tool(
            "add_processor",
            serde_json::json!({
                "name": "CameraProcessor"
            }),
            registry,
            None, // Discovery mode
        )
        .await;

        assert!(result.is_err()); // Should fail because runtime is None
    }

    #[tokio::test]
    async fn test_execute_invalid_arguments() {
        use crate::core::ProcessorRegistry;
        let registry = Arc::new(Mutex::new(ProcessorRegistry::new()));
        let result = execute_tool(
            "add_processor",
            serde_json::json!({
                "invalid": "arguments"
            }),
            registry,
            None, // Discovery mode
        )
        .await;

        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_list_processors_requires_runtime() {
        use crate::core::ProcessorRegistry;
        let registry = Arc::new(Mutex::new(ProcessorRegistry::new()));
        let result = execute_tool("list_processors", serde_json::json!({}), registry, None).await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, McpError::Runtime(_)));
    }

    #[tokio::test]
    async fn test_list_connections_requires_runtime() {
        use crate::core::ProcessorRegistry;
        let registry = Arc::new(Mutex::new(ProcessorRegistry::new()));
        let result = execute_tool("list_connections", serde_json::json!({}), registry, None).await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, McpError::Runtime(_)));
    }
}
