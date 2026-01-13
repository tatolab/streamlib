// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! MCP (Model Context Protocol) server for Claude Code integration.
//!
//! Provides structured access to broker status, runtimes, connections, and logs.

use std::io::{BufRead, Write};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use streamlib_broker::proto::broker_service_client::BrokerServiceClient;
use streamlib_broker::proto::{
    GetHealthRequest, GetVersionRequest, ListConnectionsRequest, ListProcessorsRequest,
    ListRuntimesRequest,
};
use streamlib_broker::GRPC_PORT;

// ============================================================================
// JSON-RPC Types
// ============================================================================

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct JsonRpcRequest {
    jsonrpc: String,
    id: Option<Value>,
    method: String,
    #[serde(default)]
    params: Value,
}

#[derive(Debug, Serialize)]
struct JsonRpcResponse {
    jsonrpc: String,
    id: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<JsonRpcError>,
}

#[derive(Debug, Serialize)]
struct JsonRpcError {
    code: i32,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    data: Option<Value>,
}

impl JsonRpcResponse {
    fn success(id: Value, result: Value) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            id,
            result: Some(result),
            error: None,
        }
    }

    fn error(id: Value, code: i32, message: impl Into<String>) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            id,
            result: None,
            error: Some(JsonRpcError {
                code,
                message: message.into(),
                data: None,
            }),
        }
    }
}

// ============================================================================
// MCP Types
// ============================================================================

#[derive(Debug, Serialize)]
struct ServerInfo {
    name: String,
    version: String,
}

#[derive(Debug, Serialize)]
struct ServerCapabilities {
    tools: ToolsCapability,
}

#[derive(Debug, Serialize)]
struct ToolsCapability {
    #[serde(rename = "listChanged")]
    list_changed: bool,
}

#[derive(Debug, Serialize)]
struct Tool {
    name: String,
    description: String,
    #[serde(rename = "inputSchema")]
    input_schema: Value,
}

// ============================================================================
// MCP Server
// ============================================================================

/// Run the MCP server on stdio.
pub async fn serve() -> Result<()> {
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();

    // Disable logging to stderr for clean JSON-RPC communication
    // (logs would interfere with the protocol)

    for line in stdin.lock().lines() {
        let line = line.context("Failed to read from stdin")?;
        if line.is_empty() {
            continue;
        }

        let request: JsonRpcRequest = match serde_json::from_str(&line) {
            Ok(req) => req,
            Err(e) => {
                let response =
                    JsonRpcResponse::error(Value::Null, -32700, format!("Parse error: {}", e));
                writeln!(stdout, "{}", serde_json::to_string(&response)?)?;
                stdout.flush()?;
                continue;
            }
        };

        let response = handle_request(request).await;
        writeln!(stdout, "{}", serde_json::to_string(&response)?)?;
        stdout.flush()?;
    }

    Ok(())
}

async fn handle_request(request: JsonRpcRequest) -> JsonRpcResponse {
    let id = request.id.clone().unwrap_or(Value::Null);

    match request.method.as_str() {
        "initialize" => handle_initialize(id),
        "notifications/initialized" => {
            // Client acknowledges initialization - no response needed for notifications
            // but we return empty result since we have an id
            JsonRpcResponse::success(id, json!({}))
        }
        "tools/list" => handle_tools_list(id),
        "tools/call" => handle_tools_call(id, request.params).await,
        _ => JsonRpcResponse::error(id, -32601, format!("Method not found: {}", request.method)),
    }
}

fn handle_initialize(id: Value) -> JsonRpcResponse {
    JsonRpcResponse::success(
        id,
        json!({
            "protocolVersion": "2024-11-05",
            "serverInfo": ServerInfo {
                name: "streamlib".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
            },
            "capabilities": ServerCapabilities {
                tools: ToolsCapability {
                    list_changed: false,
                },
            },
        }),
    )
}

fn handle_tools_list(id: Value) -> JsonRpcResponse {
    let tools = vec![
        Tool {
            name: "broker_status".to_string(),
            description: "Get broker health, version, and uptime status".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {},
                "required": []
            }),
        },
        Tool {
            name: "broker_runtimes".to_string(),
            description: "List all registered StreamLib runtimes".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {},
                "required": []
            }),
        },
        Tool {
            name: "broker_processors".to_string(),
            description: "List registered processors (subprocess bridges)".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "runtime_id": {
                        "type": "string",
                        "description": "Filter by runtime ID (optional)"
                    }
                },
                "required": []
            }),
        },
        Tool {
            name: "broker_connections".to_string(),
            description: "List active XPC connections between runtimes and subprocesses"
                .to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "runtime_id": {
                        "type": "string",
                        "description": "Filter by runtime ID (optional)"
                    }
                },
                "required": []
            }),
        },
        Tool {
            name: "broker_logs".to_string(),
            description: "Get recent broker log entries from /tmp/streamlib-broker.log".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "lines": {
                        "type": "integer",
                        "description": "Number of lines to return (default: 50)",
                        "default": 50
                    }
                },
                "required": []
            }),
        },
        Tool {
            name: "broker_install".to_string(),
            description: "Install or reinstall the broker service".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "force": {
                        "type": "boolean",
                        "description": "Force reinstall even if already installed",
                        "default": false
                    }
                },
                "required": []
            }),
        },
    ];

    JsonRpcResponse::success(id, json!({ "tools": tools }))
}

async fn handle_tools_call(id: Value, params: Value) -> JsonRpcResponse {
    let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");

    let arguments = params.get("arguments").cloned().unwrap_or(json!({}));

    let result = match name {
        "broker_status" => tool_broker_status().await,
        "broker_runtimes" => tool_broker_runtimes().await,
        "broker_processors" => {
            let runtime_id = arguments.get("runtime_id").and_then(|v| v.as_str());
            tool_broker_processors(runtime_id).await
        }
        "broker_connections" => {
            let runtime_id = arguments.get("runtime_id").and_then(|v| v.as_str());
            tool_broker_connections(runtime_id).await
        }
        "broker_logs" => {
            let lines = arguments
                .get("lines")
                .and_then(|v| v.as_u64())
                .unwrap_or(50) as usize;
            tool_broker_logs(lines).await
        }
        "broker_install" => {
            let force = arguments
                .get("force")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            tool_broker_install(force).await
        }
        _ => Err(anyhow::anyhow!("Unknown tool: {}", name)),
    };

    match result {
        Ok(text) => JsonRpcResponse::success(
            id,
            json!({
                "content": [{
                    "type": "text",
                    "text": text
                }]
            }),
        ),
        Err(e) => JsonRpcResponse::success(
            id,
            json!({
                "content": [{
                    "type": "text",
                    "text": format!("Error: {}", e)
                }],
                "isError": true
            }),
        ),
    }
}

// ============================================================================
// Tool Implementations
// ============================================================================

fn broker_endpoint() -> String {
    format!("http://127.0.0.1:{}", GRPC_PORT)
}

async fn tool_broker_status() -> Result<String> {
    let endpoint = broker_endpoint();
    let mut client = BrokerServiceClient::connect(endpoint.clone())
        .await
        .context(
            "Failed to connect to broker. Is the broker running? Run: streamlib broker install",
        )?;

    let health = client
        .get_health(GetHealthRequest {})
        .await
        .context("Failed to get broker health")?
        .into_inner();

    let version = client
        .get_version(GetVersionRequest {})
        .await
        .context("Failed to get broker version")?
        .into_inner();

    let status = json!({
        "endpoint": endpoint,
        "healthy": health.healthy,
        "status": health.status,
        "uptime_secs": health.uptime_secs,
        "version": version.version,
        "git_commit": version.git_commit,
        "build_date": version.build_date,
        "protocol_version": version.protocol_version
    });

    Ok(serde_json::to_string_pretty(&status)?)
}

async fn tool_broker_runtimes() -> Result<String> {
    let endpoint = broker_endpoint();
    let mut client = BrokerServiceClient::connect(endpoint)
        .await
        .context("Failed to connect to broker")?;

    let response = client
        .list_runtimes(ListRuntimesRequest {})
        .await
        .context("Failed to list runtimes")?
        .into_inner();

    let runtimes: Vec<Value> = response
        .runtimes
        .iter()
        .map(|r| {
            json!({
                "runtime_id": r.runtime_id,
                "processor_count": r.processor_count,
                "connection_count": r.connection_count,
                "age_ms": r.registered_at_unix_ms
            })
        })
        .collect();

    let result = json!({
        "count": runtimes.len(),
        "runtimes": runtimes
    });

    Ok(serde_json::to_string_pretty(&result)?)
}

async fn tool_broker_processors(runtime_id: Option<&str>) -> Result<String> {
    let endpoint = broker_endpoint();
    let mut client = BrokerServiceClient::connect(endpoint)
        .await
        .context("Failed to connect to broker")?;

    let response = client
        .list_processors(ListProcessorsRequest {
            runtime_id: runtime_id.unwrap_or("").to_string(),
        })
        .await
        .context("Failed to list processors")?
        .into_inner();

    let processors: Vec<Value> = response
        .processors
        .iter()
        .map(|p| {
            json!({
                "processor_id": p.processor_id,
                "runtime_id": p.runtime_id,
                "processor_type": p.processor_type,
                "bridge_state": p.bridge_state,
                "age_ms": p.registered_at_unix_ms
            })
        })
        .collect();

    let result = json!({
        "count": processors.len(),
        "filter": runtime_id,
        "processors": processors
    });

    Ok(serde_json::to_string_pretty(&result)?)
}

async fn tool_broker_connections(runtime_id: Option<&str>) -> Result<String> {
    let endpoint = broker_endpoint();
    let mut client = BrokerServiceClient::connect(endpoint)
        .await
        .context("Failed to connect to broker")?;

    let response = client
        .list_connections(ListConnectionsRequest {
            runtime_id: runtime_id.unwrap_or("").to_string(),
        })
        .await
        .context("Failed to list connections")?
        .into_inner();

    let connections: Vec<Value> = response
        .connections
        .iter()
        .map(|c| {
            json!({
                "connection_id": c.connection_id,
                "runtime_id": c.runtime_id,
                "processor_id": c.processor_id,
                "role": c.role,
                "age_ms": c.established_at_unix_ms,
                "frames_transferred": c.frames_transferred,
                "bytes_transferred": c.bytes_transferred
            })
        })
        .collect();

    let result = json!({
        "count": connections.len(),
        "filter": runtime_id,
        "connections": connections
    });

    Ok(serde_json::to_string_pretty(&result)?)
}

async fn tool_broker_logs(lines: usize) -> Result<String> {
    use std::process::Command;

    let output = Command::new("tail")
        .args(["-n", &lines.to_string(), "/tmp/streamlib-broker.log"])
        .output()
        .context("Failed to read broker logs")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("No such file") {
            return Ok(json!({
                "error": "Log file not found. Broker may not have run yet.",
                "path": "/tmp/streamlib-broker.log"
            })
            .to_string());
        }
        anyhow::bail!("Failed to read logs: {}", stderr);
    }

    let log_content = String::from_utf8_lossy(&output.stdout);
    let log_lines: Vec<&str> = log_content.lines().collect();

    let result = json!({
        "path": "/tmp/streamlib-broker.log",
        "lines_requested": lines,
        "lines_returned": log_lines.len(),
        "content": log_content.trim()
    });

    Ok(serde_json::to_string_pretty(&result)?)
}

async fn tool_broker_install(force: bool) -> Result<String> {
    use super::broker;

    broker::install(force, None).await?;

    Ok(json!({
        "success": true,
        "force": force,
        "message": "Broker installed successfully"
    })
    .to_string())
}
