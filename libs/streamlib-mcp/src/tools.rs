//! MCP Tools - Runtime Actions
//!
//! Tools expose actions that AI agents can invoke to modify the runtime.
//! Examples: add_processor, remove_processor, connect_processors

use crate::{McpError, Result};
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
            name: "add_processor".to_string(),
            description: "Add a processor to the runtime by name. The processor must be registered in the registry.".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "description": "Name of the processor to add (e.g., 'CameraProcessor', 'DisplayProcessor')"
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

/// Arguments for add_processor tool
#[derive(Debug, Deserialize)]
pub struct AddProcessorArgs {
    pub name: String,
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
        "add_processor" => {
            let args: AddProcessorArgs = serde_json::from_value(arguments)
                .map_err(|e| McpError::InvalidArguments {
                    tool: tool_name.to_string(),
                    message: e.to_string(),
                })?;

            // TODO: Implement actual add_processor logic
            // This will require access to StreamRuntime

            Ok(ToolResult {
                success: false,
                message: format!(
                    "add_processor('{}') not yet implemented - placeholder only",
                    args.name
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
        assert_eq!(tools.len(), 4);

        let tool_names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
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
