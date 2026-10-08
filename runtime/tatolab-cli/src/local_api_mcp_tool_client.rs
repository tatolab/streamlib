// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `tatolab`'s MCP client of a running runtime: `rmcp`'s client over the runtime's local API
//! socket, at the latest revision.

use std::path::{Path, PathBuf};
use std::time::Duration;

use rmcp::RoleClient;
use rmcp::model::{CallToolRequestParams, CallToolResult, ContentBlock, ProtocolVersion};
use rmcp::service::{
    ClientInitializeError, ClientLifecycleMode, ClientServiceExt, RunningService, ServiceError,
};
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use rmcp::transport::{StreamableHttpClientTransport, UnixSocketHttpClient};

use crate::TatolabCommandFailure;

/// The URI the client addresses; it fills `Host`, and the socket path is the address.
const LOCAL_API_MCP_URI: &str = "http://localhost/mcp";

/// Bounds a liveness round trip, so a socket that accepts but never answers cannot stall a
/// registry scan.
pub(crate) const LOCAL_API_LIVENESS_ROUND_TRIP_TIMEOUT: Duration = Duration::from_millis(1500);

/// Bounds an observation verb's tool call: `tap` and `logs` fill a bounded sample runtime-side
/// and can take a moment to.
pub(crate) const OBSERVATION_VERB_TOOL_CALL_TIMEOUT: Duration = Duration::from_secs(30);

/// Why an MCP request to a runtime's local API came back without a result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LocalApiMcpToolClientFailure {
    /// Nothing answered MCP on the socket, or the answer did not come in time.
    LocalApiUnreachable { failure_description: String },
    /// The runtime answered and refused: a JSON-RPC error, no protocol revision in common, or an
    /// answer that was not the request's result.
    RequestRefusedByTheRuntime { failure_description: String },
    /// The tool ran and reported a failure, or answered with no text.
    ToolCallFailed { failure_description: String },
}

impl LocalApiMcpToolClientFailure {
    /// Whether the runtime answered, refusing or failing the call, rather than nothing answering.
    pub(crate) fn the_runtime_answered(&self) -> bool {
        !matches!(self, Self::LocalApiUnreachable { .. })
    }

    fn failure_description(&self) -> &str {
        match self {
            Self::LocalApiUnreachable {
                failure_description,
            }
            | Self::RequestRefusedByTheRuntime {
                failure_description,
            }
            | Self::ToolCallFailed {
                failure_description,
            } => failure_description,
        }
    }
}

impl std::fmt::Display for LocalApiMcpToolClientFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.failure_description())
    }
}

impl std::error::Error for LocalApiMcpToolClientFailure {}

impl From<LocalApiMcpToolClientFailure> for TatolabCommandFailure {
    fn from(local_api_mcp_tool_client_failure: LocalApiMcpToolClientFailure) -> Self {
        TatolabCommandFailure::refused(local_api_mcp_tool_client_failure.to_string())
    }
}

/// A connected MCP client of one runtime's local API; closes its connection when dropped.
pub(crate) struct LocalApiMcpToolClient {
    connected_mcp_client: Option<RunningService<RoleClient, ()>>,
    client_tokio_runtime: tokio::runtime::Runtime,
    local_api_socket_path: PathBuf,
    request_timeout: Duration,
}

impl LocalApiMcpToolClient {
    /// Connect to the local API at `local_api_socket_path` through `server/discover`, bounding
    /// the connect and every later request by `request_timeout`.
    pub(crate) fn connect(
        local_api_socket_path: &Path,
        request_timeout: Duration,
    ) -> Result<Self, LocalApiMcpToolClientFailure> {
        let local_api_socket_path_text = local_api_socket_path.display().to_string();
        let socket_path_to_dial = socket_path_to_dial(local_api_socket_path).ok_or_else(|| {
            LocalApiMcpToolClientFailure::LocalApiUnreachable {
                failure_description: format!(
                    "no runtime answers MCP at {local_api_socket_path_text} (not a socket path \
                     this client can dial)"
                ),
            }
        })?;
        let client_tokio_runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(
                |runtime_start_failure| LocalApiMcpToolClientFailure::LocalApiUnreachable {
                    failure_description: format!(
                        "could not start the MCP client's runtime: {runtime_start_failure}"
                    ),
                },
            )?;
        let connected = client_tokio_runtime.block_on(async {
            // The transport spawns its worker as it is built, so it is built inside the runtime
            // that drives it.
            let transport = StreamableHttpClientTransport::with_client(
                UnixSocketHttpClient::new(&socket_path_to_dial, LOCAL_API_MCP_URI),
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
        });
        let connected_mcp_client = match connected {
            Ok(Ok(connected_mcp_client)) => connected_mcp_client,
            Ok(Err(ClientInitializeError::JsonRpcError(refusal))) => {
                return Err(LocalApiMcpToolClientFailure::RequestRefusedByTheRuntime {
                    failure_description: format!(
                        "the runtime at {local_api_socket_path_text} refused `server/discover`: \
                         {} ({})",
                        refusal.message, refusal.code.0
                    ),
                });
            }
            Ok(Err(ClientInitializeError::NoCompatibleProtocolVersion {
                server_supported,
                ..
            })) => {
                return Err(LocalApiMcpToolClientFailure::RequestRefusedByTheRuntime {
                    failure_description: format!(
                        "the runtime at {local_api_socket_path_text} serves {server_supported:?}, \
                         not {}",
                        ProtocolVersion::LATEST
                    ),
                });
            }
            Ok(Err(connect_failure)) => {
                return Err(LocalApiMcpToolClientFailure::LocalApiUnreachable {
                    failure_description: format!(
                        "no runtime answers MCP at {local_api_socket_path_text} ({connect_failure})"
                    ),
                });
            }
            Err(_elapsed) => {
                return Err(LocalApiMcpToolClientFailure::LocalApiUnreachable {
                    failure_description: format!(
                        "no runtime answered MCP at {local_api_socket_path_text} within \
                         {request_timeout:?}"
                    ),
                });
            }
        };
        Ok(Self {
            connected_mcp_client: Some(connected_mcp_client),
            client_tokio_runtime,
            local_api_socket_path: local_api_socket_path.to_path_buf(),
            request_timeout,
        })
    }

    /// Call `tool_name` with `tool_arguments`, answering the first text block of its result.
    pub(crate) fn call_tool(
        &self,
        tool_name: &str,
        tool_arguments: serde_json::Map<String, serde_json::Value>,
    ) -> Result<String, LocalApiMcpToolClientFailure> {
        let Some(connected_mcp_client) = self.connected_mcp_client.as_ref() else {
            return Err(LocalApiMcpToolClientFailure::LocalApiUnreachable {
                failure_description: "this MCP client is closed".to_owned(),
            });
        };
        let request =
            CallToolRequestParams::new(tool_name.to_owned()).with_arguments(tool_arguments);
        let answered = self.client_tokio_runtime.block_on(async {
            tokio::time::timeout(
                self.request_timeout,
                connected_mcp_client.call_tool(request),
            )
            .await
        });
        match answered {
            Ok(Ok(tool_result)) => first_text_block_of_tool_result(tool_name, tool_result),
            Ok(Err(service_failure)) => {
                Err(self.failure_for_a_tool_call_without_a_result(tool_name, service_failure))
            }
            Err(_elapsed) => Err(LocalApiMcpToolClientFailure::LocalApiUnreachable {
                failure_description: format!(
                    "`{tool_name}` to the runtime at {} did not answer within {:?}",
                    self.local_api_socket_path.display(),
                    self.request_timeout
                ),
            }),
        }
    }

    /// End the client's connection.
    pub(crate) fn close(mut self) {
        self.close_the_connection();
    }

    fn close_the_connection(&mut self) {
        let Some(connected_mcp_client) = self.connected_mcp_client.take() else {
            return;
        };
        // A close that fails or stalls leaves nothing to undo: the connection is per request.
        let _close_outcome = self.client_tokio_runtime.block_on(async {
            tokio::time::timeout(self.request_timeout, connected_mcp_client.cancel()).await
        });
    }

    /// The failure for a `tools/call` that did not come back as a result: a refusal when the
    /// runtime answered, unreachable when nothing did.
    fn failure_for_a_tool_call_without_a_result(
        &self,
        tool_name: &str,
        service_failure: ServiceError,
    ) -> LocalApiMcpToolClientFailure {
        match service_failure {
            ServiceError::McpError(refusal) => {
                LocalApiMcpToolClientFailure::RequestRefusedByTheRuntime {
                    failure_description: format!("{} ({})", refusal.message, refusal.code.0),
                }
            }
            ServiceError::UnexpectedResponse => {
                LocalApiMcpToolClientFailure::RequestRefusedByTheRuntime {
                    failure_description: format!(
                        "the runtime at {} answered `{tool_name}` with something other than its \
                         result",
                        self.local_api_socket_path.display()
                    ),
                }
            }
            other_failure => LocalApiMcpToolClientFailure::LocalApiUnreachable {
                failure_description: format!(
                    "`{tool_name}` to the runtime at {} failed: {other_failure}",
                    self.local_api_socket_path.display()
                ),
            },
        }
    }
}

impl Drop for LocalApiMcpToolClient {
    fn drop(&mut self) {
        self.close_the_connection();
    }
}

/// The path handed to the socket client: absolute, because `rmcp` reads a leading `@` as a Linux
/// abstract socket and refuses an empty path by panicking, where a path in a registry entry means
/// a file.
fn socket_path_to_dial(local_api_socket_path: &Path) -> Option<String> {
    let absolute_socket_path = std::path::absolute(local_api_socket_path).ok()?;
    absolute_socket_path.to_str().map(str::to_owned)
}

/// The first text block of a tool's result, or the failure it reported.
fn first_text_block_of_tool_result(
    tool_name: &str,
    tool_result: CallToolResult,
) -> Result<String, LocalApiMcpToolClientFailure> {
    let tool_reported_a_failure = tool_result.is_error.unwrap_or(false);
    let first_text =
        tool_result
            .content
            .into_iter()
            .find_map(|content_block| match content_block {
                ContentBlock::Text(text_block) => Some(text_block.text),
                _ => None,
            });
    match (tool_reported_a_failure, first_text) {
        (false, Some(first_text)) => Ok(first_text),
        (true, failure_text) => Err(LocalApiMcpToolClientFailure::ToolCallFailed {
            failure_description: format!(
                "{tool_name} failed: {}",
                failure_text.as_deref().unwrap_or("no detail given")
            ),
        }),
        (false, None) => Err(LocalApiMcpToolClientFailure::ToolCallFailed {
            failure_description: format!("{tool_name} returned no text content"),
        }),
    }
}

/// Whether a runtime on `local_api_socket_path` answers MCP at all: answering `server/discover`,
/// or refusing it in the protocol's own words, is alive.
pub(crate) fn local_api_answers_mcp(local_api_socket_path: &Path) -> bool {
    match LocalApiMcpToolClient::connect(
        local_api_socket_path,
        LOCAL_API_LIVENESS_ROUND_TRIP_TIMEOUT,
    ) {
        Ok(connected_client) => {
            connected_client.close();
            true
        }
        Err(connect_failure) => connect_failure.the_runtime_answered(),
    }
}

/// Connect, call `tool_name` once with `tool_arguments`, and close: an observation verb's one
/// round trip, each failure worded as the verb reports it.
pub(crate) fn call_one_local_api_tool(
    local_api_socket_path: &Path,
    tool_name: &str,
    tool_arguments: serde_json::Map<String, serde_json::Value>,
) -> Result<String, LocalApiMcpToolClientFailure> {
    LocalApiMcpToolClient::connect(local_api_socket_path, OBSERVATION_VERB_TOOL_CALL_TIMEOUT)
        .and_then(|connected_client| {
            let answered = connected_client.call_tool(tool_name, tool_arguments);
            connected_client.close();
            answered
        })
        .map_err(|call_failure| match call_failure {
            LocalApiMcpToolClientFailure::RequestRefusedByTheRuntime {
                failure_description,
            } => LocalApiMcpToolClientFailure::RequestRefusedByTheRuntime {
                failure_description: format!("{tool_name} failed: {failure_description}"),
            },
            unreachable_or_tool_failure => unreachable_or_tool_failure,
        })
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use serde_json::json;

    use super::*;
    use crate::isolated_node_registry::NOTHING_LISTENS_LOCAL_API_SOCKET_PATH;
    use crate::stub_local_api_server::{
        RecordedToolCall, StubLocalApiScript, StubLocalApiServer, StubToolAnswer,
    };

    fn json_object(json_value: serde_json::Value) -> serde_json::Map<String, serde_json::Value> {
        json_value.as_object().cloned().unwrap()
    }

    #[test]
    fn a_local_api_socket_that_answers_is_reachable() {
        let stub_local_api_server = StubLocalApiServer::serve_default();

        assert!(local_api_answers_mcp(
            &stub_local_api_server.local_api_socket_path
        ));
    }

    #[test]
    fn a_local_api_socket_nothing_listens_on_is_not_reachable() {
        let stale_socket_directory = tempfile::Builder::new()
            .prefix("tl-stale-")
            .tempdir_in("/tmp")
            .unwrap();
        let stale_socket_path = stale_socket_directory.path().join("local-api.sock");
        drop(std::os::unix::net::UnixListener::bind(&stale_socket_path).unwrap());
        assert!(
            stale_socket_path.exists(),
            "a closed listener leaves its file"
        );

        assert!(!local_api_answers_mcp(&stale_socket_path));
        assert!(!local_api_answers_mcp(Path::new(
            NOTHING_LISTENS_LOCAL_API_SOCKET_PATH
        )));
    }

    #[test]
    fn a_socket_that_accepts_and_never_answers_is_not_reachable_within_the_liveness_bound() {
        let silent_socket_directory = tempfile::Builder::new()
            .prefix("tl-silent-")
            .tempdir_in("/tmp")
            .unwrap();
        let silent_socket_path = silent_socket_directory.path().join("local-api.sock");
        let _never_accepting_listener =
            std::os::unix::net::UnixListener::bind(&silent_socket_path).unwrap();
        let probe_started = Instant::now();

        assert!(!local_api_answers_mcp(&silent_socket_path));
        assert!(
            probe_started.elapsed() < LOCAL_API_LIVENESS_ROUND_TRIP_TIMEOUT * 4,
            "the probe took {:?}",
            probe_started.elapsed()
        );
    }

    #[test]
    fn a_socket_path_no_client_can_dial_is_unreachable_rather_than_a_panic() {
        for undialable_socket_path in ["", "@"] {
            assert!(
                !local_api_answers_mcp(Path::new(undialable_socket_path)),
                "{undialable_socket_path:?}"
            );
        }
    }

    #[test]
    fn a_tool_call_carries_its_name_and_arguments_to_the_runtime() {
        let stub_local_api_server = StubLocalApiServer::serve_answering_every_tool_call_with(
            StubToolAnswer::tool_result(r#"{"nodes":[]}"#),
        );

        let tool_result_text = call_one_local_api_tool(
            &stub_local_api_server.local_api_socket_path,
            "tap",
            json_object(json!({"channel": "cam/video", "count": 4})),
        )
        .unwrap();

        assert_eq!(tool_result_text, r#"{"nodes":[]}"#);
        assert_eq!(
            stub_local_api_server.recorded_tool_calls(),
            [RecordedToolCall {
                tool_name: "tap".to_owned(),
                tool_arguments: json!({"channel": "cam/video", "count": 4}),
            }]
        );
    }

    #[test]
    fn one_connected_client_answers_queued_rounds_in_order_then_the_fixed_answer() {
        let stub_local_api_server = StubLocalApiServer::serve(StubLocalApiScript {
            fixed_tool_answer: Some(StubToolAnswer::tool_result("fixed")),
            queued_tool_answers: vec![
                StubToolAnswer::tool_result("first"),
                StubToolAnswer::tool_result("second"),
            ],
            ..StubLocalApiScript::default()
        });
        let connected_client = LocalApiMcpToolClient::connect(
            &stub_local_api_server.local_api_socket_path,
            OBSERVATION_VERB_TOOL_CALL_TIMEOUT,
        )
        .unwrap();

        let answers: Vec<String> = (0..3)
            .map(|_| {
                connected_client
                    .call_tool("graph", serde_json::Map::new())
                    .unwrap()
            })
            .collect();
        connected_client.close();

        assert_eq!(answers, ["first", "second", "fixed"]);
        assert_eq!(stub_local_api_server.recorded_tool_calls().len(), 3);
    }

    #[test]
    fn a_tool_level_error_is_raised_not_returned_as_a_result() {
        let stub_local_api_server = StubLocalApiServer::serve_answering_every_tool_call_with(
            StubToolAnswer::tool_failure("no such channel"),
        );

        let tool_failure = call_one_local_api_tool(
            &stub_local_api_server.local_api_socket_path,
            "tap",
            json_object(json!({"channel": "nope"})),
        )
        .unwrap_err();

        assert_eq!(
            tool_failure,
            LocalApiMcpToolClientFailure::ToolCallFailed {
                failure_description: "tap failed: no such channel".to_owned()
            }
        );
        assert!(tool_failure.the_runtime_answered());
    }

    #[test]
    fn a_call_the_runtime_refuses_is_raised_naming_the_refusal() {
        let stub_local_api_server = StubLocalApiServer::serve(StubLocalApiScript {
            refuse_every_tool_call_with: Some("no tool named `nope`".to_owned()),
            ..StubLocalApiScript::default()
        });

        let refusal = call_one_local_api_tool(
            &stub_local_api_server.local_api_socket_path,
            "nope",
            serde_json::Map::new(),
        )
        .unwrap_err();

        assert_eq!(
            refusal,
            LocalApiMcpToolClientFailure::RequestRefusedByTheRuntime {
                failure_description: "nope failed: no tool named `nope` (-32602)".to_owned()
            }
        );
        assert!(refusal.the_runtime_answered());
    }

    #[test]
    fn an_unreachable_local_api_socket_is_named() {
        let unreachable = call_one_local_api_tool(
            Path::new(NOTHING_LISTENS_LOCAL_API_SOCKET_PATH),
            "graph",
            serde_json::Map::new(),
        )
        .unwrap_err();

        assert!(
            matches!(
                unreachable,
                LocalApiMcpToolClientFailure::LocalApiUnreachable { .. }
            ),
            "{unreachable:?}"
        );
        assert!(!unreachable.the_runtime_answered());
        let unreachable_message = unreachable.to_string();
        assert!(
            unreachable_message.starts_with(&format!(
                "no runtime answers MCP at {NOTHING_LISTENS_LOCAL_API_SOCKET_PATH} ("
            )),
            "{unreachable_message}"
        );
    }

    #[test]
    fn a_tool_result_with_no_text_is_a_failure_and_a_failure_with_no_text_says_so() {
        assert_eq!(
            first_text_block_of_tool_result("graph", CallToolResult::success(Vec::new())),
            Err(LocalApiMcpToolClientFailure::ToolCallFailed {
                failure_description: "graph returned no text content".to_owned()
            })
        );
        assert_eq!(
            first_text_block_of_tool_result("graph", CallToolResult::error(Vec::new())),
            Err(LocalApiMcpToolClientFailure::ToolCallFailed {
                failure_description: "graph failed: no detail given".to_owned()
            })
        );
    }

    #[test]
    fn a_runtime_that_refuses_in_the_protocols_own_words_answered_and_one_that_is_silent_did_not() {
        let failure_description = String::new();
        assert!(
            LocalApiMcpToolClientFailure::RequestRefusedByTheRuntime {
                failure_description: failure_description.clone()
            }
            .the_runtime_answered()
        );
        assert!(
            LocalApiMcpToolClientFailure::ToolCallFailed {
                failure_description: failure_description.clone()
            }
            .the_runtime_answered()
        );
        assert!(
            !LocalApiMcpToolClientFailure::LocalApiUnreachable {
                failure_description
            }
            .the_runtime_answered()
        );
    }
}
