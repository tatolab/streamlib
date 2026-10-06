// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The wheel's MCP client of a running node: `rmcp`'s client over the node's
//! local API socket, at the latest revision.

use std::time::Duration;

use parking_lot::Mutex;
use pyo3::create_exception;
use pyo3::exceptions::{PyException, PyRuntimeError, PyValueError};
use pyo3::prelude::*;
use rmcp::RoleClient;
use rmcp::model::{
    CallToolRequestParams, CallToolResult, ContentBlock, JsonObject, ProtocolVersion,
};
use rmcp::service::{
    ClientInitializeError, ClientLifecycleMode, ClientServiceExt, RunningService, ServiceError,
};
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use rmcp::transport::{StreamableHttpClientTransport, UnixSocketHttpClient};

/// The URI the client addresses; it fills `Host`, and the socket path is the
/// address.
const LOCAL_API_MCP_URI: &str = "http://localhost/mcp";

create_exception!(
    _engine,
    LocalApiMcpServerUnreachable,
    PyException,
    "Nothing answered MCP on the local API socket."
);
create_exception!(
    _engine,
    LocalApiMcpRequestRefused,
    PyException,
    "The node answered and refused the MCP request."
);
create_exception!(
    _engine,
    LocalApiMcpToolCallFailed,
    PyException,
    "The tool ran and reported a failure, or answered with no text."
);

/// A connected MCP client of one node's local API.
#[pyclass(name = "LocalApiMcpClient", module = "streamlib._engine", frozen)]
pub(crate) struct PythonLocalApiMcpClient {
    // Declared before the runtime so it is dropped while the runtime that
    // drives it still exists.
    connected_mcp_client: Mutex<Option<RunningService<RoleClient, ()>>>,
    tokio_runtime: tokio::runtime::Runtime,
    local_api_socket_path: String,
    request_timeout: Duration,
}

#[pymethods]
impl PythonLocalApiMcpClient {
    #[new]
    fn new(
        python: Python<'_>,
        local_api_socket_path: String,
        timeout_seconds: f64,
    ) -> PyResult<Self> {
        let request_timeout = Duration::try_from_secs_f64(timeout_seconds)
            .ok()
            .filter(|timeout| !timeout.is_zero())
            .ok_or_else(|| {
                PyValueError::new_err(format!(
                    "timeout_seconds must be a positive number of seconds, got {timeout_seconds}"
                ))
            })?;
        let tokio_runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| {
                PyRuntimeError::new_err(format!("could not start the MCP client's runtime: {e}"))
            })?;
        let connected = python.detach(|| {
            tokio_runtime.block_on(async {
                // The transport spawns its worker as it is built, so it is
                // built inside the runtime that drives it.
                let transport = StreamableHttpClientTransport::with_client(
                    UnixSocketHttpClient::new(&local_api_socket_path, LOCAL_API_MCP_URI),
                    StreamableHttpClientTransportConfig::with_uri(LOCAL_API_MCP_URI),
                );
                tokio::time::timeout(
                    request_timeout,
                    ().serve_with_lifecycle(
                        transport,
                        ClientLifecycleMode::Discover {
                            preferred_versions: vec![ProtocolVersion::LATEST],
                        },
                    ),
                )
                .await
            })
        });
        let connected_mcp_client = match connected {
            Ok(Ok(connected_mcp_client)) => connected_mcp_client,
            Ok(Err(ClientInitializeError::JsonRpcError(refusal))) => {
                return Err(LocalApiMcpRequestRefused::new_err(format!(
                    "the node at {local_api_socket_path} refused `server/discover`: {} ({})",
                    refusal.message, refusal.code.0
                )));
            }
            Ok(Err(ClientInitializeError::NoCompatibleProtocolVersion {
                server_supported,
                ..
            })) => {
                return Err(LocalApiMcpRequestRefused::new_err(format!(
                    "the node at {local_api_socket_path} serves {server_supported:?}, not {}",
                    ProtocolVersion::LATEST
                )));
            }
            Ok(Err(connect_failure)) => {
                return Err(LocalApiMcpServerUnreachable::new_err(format!(
                    "no node answers MCP at {local_api_socket_path} ({connect_failure})"
                )));
            }
            Err(_elapsed) => {
                return Err(LocalApiMcpServerUnreachable::new_err(format!(
                    "no node answered MCP at {local_api_socket_path} within {timeout_seconds}s"
                )));
            }
        };
        Ok(Self {
            connected_mcp_client: Mutex::new(Some(connected_mcp_client)),
            tokio_runtime,
            local_api_socket_path,
            request_timeout,
        })
    }

    /// Call `tool_name` with the JSON object `arguments_json`, answering the
    /// text the tool's result carries.
    fn call_tool(
        &self,
        python: Python<'_>,
        tool_name: String,
        arguments_json: &str,
    ) -> PyResult<String> {
        let arguments: JsonObject = serde_json::from_str(arguments_json).map_err(|e| {
            PyValueError::new_err(format!(
                "`{tool_name}` arguments are not a JSON object: {e}"
            ))
        })?;
        let request = CallToolRequestParams::new(tool_name.clone()).with_arguments(arguments);
        // The lock is taken with the GIL released, so a second thread calling
        // in waits on the lock without holding the GIL this call needs back.
        let answered = python.detach(|| {
            let connected_mcp_client = self.connected_mcp_client.lock();
            let connected_mcp_client = connected_mcp_client.as_ref()?;
            Some(self.tokio_runtime.block_on(async {
                tokio::time::timeout(
                    self.request_timeout,
                    connected_mcp_client.call_tool(request),
                )
                .await
            }))
        });
        match answered {
            None => Err(LocalApiMcpServerUnreachable::new_err(
                "this MCP client is closed",
            )),
            Some(Ok(Ok(result))) => tool_result_text(&tool_name, result),
            Some(Ok(Err(failure))) => Err(self.python_error_for_service_error(&tool_name, failure)),
            Some(Err(_elapsed)) => Err(LocalApiMcpServerUnreachable::new_err(format!(
                "`{tool_name}` to the node at {} did not answer within {:?}",
                self.local_api_socket_path, self.request_timeout
            ))),
        }
    }

    /// End the client's connection. Idempotent.
    fn close(&self, python: Python<'_>) {
        python.detach(|| {
            let Some(connected_mcp_client) = self.connected_mcp_client.lock().take() else {
                return;
            };
            self.tokio_runtime.block_on(async {
                match tokio::time::timeout(self.request_timeout, connected_mcp_client.cancel())
                    .await
                {
                    Ok(Ok(_quit_reason)) => {}
                    Ok(Err(failure)) => {
                        tracing::debug!(%failure, "closing an MCP client of the local API");
                    }
                    Err(_elapsed) => {
                        tracing::debug!("closing an MCP client of the local API timed out");
                    }
                }
            })
        });
    }

    fn __enter__(this: Bound<'_, Self>) -> Bound<'_, Self> {
        this
    }

    #[pyo3(signature = (*_exception_details))]
    fn __exit__(&self, python: Python<'_>, _exception_details: &Bound<'_, PyAny>) -> bool {
        self.close(python);
        false
    }
}

impl PythonLocalApiMcpClient {
    /// The Python exception for a `tools/call` that did not come back as a
    /// result: a refusal when the node answered, unreachable when nothing did.
    fn python_error_for_service_error(&self, tool_name: &str, failure: ServiceError) -> PyErr {
        match failure {
            ServiceError::McpError(refusal) => LocalApiMcpRequestRefused::new_err(format!(
                "{} ({})",
                refusal.message, refusal.code.0
            )),
            ServiceError::UnexpectedResponse => LocalApiMcpRequestRefused::new_err(format!(
                "the node at {} answered `{tool_name}` with something other than its result",
                self.local_api_socket_path
            )),
            other => LocalApiMcpServerUnreachable::new_err(format!(
                "`{tool_name}` to the node at {} failed: {other}",
                self.local_api_socket_path
            )),
        }
    }
}

/// The first text block of a tool's result, or the failure it reported.
fn tool_result_text(tool_name: &str, result: CallToolResult) -> PyResult<String> {
    let is_error = result.is_error.unwrap_or(false);
    let text = result.content.into_iter().find_map(|block| match block {
        ContentBlock::Text(text_block) => Some(text_block.text),
        _ => None,
    });
    match (is_error, text) {
        (false, Some(text)) => Ok(text),
        (true, text) => Err(LocalApiMcpToolCallFailed::new_err(format!(
            "{tool_name} failed: {}",
            text.as_deref().unwrap_or("no detail given")
        ))),
        (false, None) => Err(LocalApiMcpToolCallFailed::new_err(format!(
            "{tool_name} returned no text content"
        ))),
    }
}

/// Add the client class and its exceptions to `_engine`.
pub(crate) fn register_local_api_mcp_client(module: &Bound<'_, PyModule>) -> PyResult<()> {
    let python = module.py();
    module.add_class::<PythonLocalApiMcpClient>()?;
    module.add(
        "LocalApiMcpServerUnreachable",
        python.get_type::<LocalApiMcpServerUnreachable>(),
    )?;
    module.add(
        "LocalApiMcpRequestRefused",
        python.get_type::<LocalApiMcpRequestRefused>(),
    )?;
    module.add(
        "LocalApiMcpToolCallFailed",
        python.get_type::<LocalApiMcpToolCallFailed>(),
    )?;
    Ok(())
}
