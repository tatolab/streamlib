//! MCP Tools - Runtime Actions
//!
//! Tools expose actions that AI agents can invoke to modify the runtime.
//! Examples: add_processor, remove_processor, connect_processors

use super::{McpError, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;

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
            description: "Connect two processors by linking an output port to an input port. Data will flow from source to destination.".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "source": {
                        "type": "string",
                        "description": "Source processor and port (e.g., 'CameraProcessor.video')"
                    },
                    "destination": {
                        "type": "string",
                        "description": "Destination processor and port (e.g., 'DisplayProcessor.video')"
                    }
                },
                "required": ["source", "destination"]
            }),
        },
        Tool {
            name: "list_processors".to_string(),
            description: "List all processors currently in the runtime (not just available in registry).".to_string(),
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
/// This is a placeholder implementation. In a real system, this would
/// interact with the StreamRuntime to actually perform the operations.
pub async fn execute_tool(
    tool_name: &str,
    arguments: JsonValue,
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
            let args: AddProcessorArgs = serde_json::from_value(arguments)
                .map_err(|e| McpError::InvalidArguments {
                    tool: tool_name.to_string(),
                    message: e.to_string(),
                })?;

            // TODO: Implement actual add_processor logic
            // This will require access to StreamRuntime

            let processor_type = match (&args.language, &args.code) {
                (Some(lang), Some(_code)) => format!("dynamic {} processor", lang),
                (None, None) => "pre-compiled processor".to_string(),
                _ => return Err(McpError::InvalidArguments {
                    tool: tool_name.to_string(),
                    message: "Must provide both 'language' and 'code' for dynamic processors, or neither for pre-compiled".to_string(),
                }),
            };

            Ok(ToolResult {
                success: false,
                message: format!(
                    "add_processor('{}', {}) not yet implemented - placeholder only",
                    args.name,
                    processor_type
                ),
                data: None,
            })
        }

        "remove_processor" => {
            let args: RemoveProcessorArgs = serde_json::from_value(arguments)
                .map_err(|e| McpError::InvalidArguments {
                    tool: tool_name.to_string(),
                    message: e.to_string(),
                })?;

            // TODO: Implement actual remove_processor logic

            Ok(ToolResult {
                success: false,
                message: format!(
                    "remove_processor('{}') not yet implemented - placeholder only",
                    args.name
                ),
                data: None,
            })
        }

        "connect_processors" => {
            let args: ConnectProcessorsArgs = serde_json::from_value(arguments)
                .map_err(|e| McpError::InvalidArguments {
                    tool: tool_name.to_string(),
                    message: e.to_string(),
                })?;

            // TODO: Implement actual connect_processors logic

            Ok(ToolResult {
                success: false,
                message: format!(
                    "connect_processors('{}' -> '{}') not yet implemented - placeholder only",
                    args.source, args.destination
                ),
                data: None,
            })
        }

        "list_processors" => {
            // TODO: Implement actual list_processors logic
            // Should return processors currently in runtime

            Ok(ToolResult {
                success: false,
                message: "list_processors not yet implemented - placeholder only".to_string(),
                data: Some(serde_json::json!({
                    "processors": []
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
        assert_eq!(tools.len(), 8);

        let tool_names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
        assert!(tool_names.contains(&"list_supported_languages"));
        assert!(tool_names.contains(&"list_packages"));
        assert!(tool_names.contains(&"request_package"));
        assert!(tool_names.contains(&"get_package_status"));
        assert!(tool_names.contains(&"add_processor"));
        assert!(tool_names.contains(&"remove_processor"));
        assert!(tool_names.contains(&"connect_processors"));
        assert!(tool_names.contains(&"list_processors"));
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
        let result = execute_tool("unknown_tool", serde_json::json!({})).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_execute_add_processor_placeholder() {
        let result = execute_tool(
            "add_processor",
            serde_json::json!({
                "name": "CameraProcessor"
            }),
        )
        .await;

        assert!(result.is_ok());
        let result = result.unwrap();
        assert!(!result.success); // Placeholder returns false
        assert!(result.message.contains("not yet implemented"));
    }

    #[tokio::test]
    async fn test_execute_invalid_arguments() {
        let result = execute_tool(
            "add_processor",
            serde_json::json!({
                "invalid": "arguments"
            }),
        )
        .await;

        assert!(result.is_err());
    }
}
