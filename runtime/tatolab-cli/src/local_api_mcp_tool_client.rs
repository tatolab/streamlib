// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `tatolab`'s MCP client of the machine's runtime: `rmcp`'s client over the runtime's local API
//! socket, at the latest revision — one request per call over `POST /mcp`, or a connection of its
//! own over the `/mcp/stdio` upgrade, which an attached stream lives as long as.

use std::path::{Path, PathBuf};
use std::time::Duration;

use rmcp::RoleClient;
use rmcp::model::{CallToolRequestParams, CallToolResult, ContentBlock, ProtocolVersion};
use rmcp::service::{
    ClientInitializeError, ClientLifecycleMode, ClientServiceExt, RunningService, ServiceError,
};
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use rmcp::transport::{StreamableHttpClientTransport, UnixSocketHttpClient};
use streamlib_runtime_client_contract::local_api_wire_contract::MCP_STREAMABLE_HTTP_ROUTE_PATH;
use tokio::io::AsyncReadExt;

use crate::TatolabCommandFailure;
use crate::local_api_connection::LocalApiConnection;
use crate::local_api_unix_socket_http_client::{
    UpgradedLocalApiMcpStdioStream, upgrade_local_api_connection_to_mcp_stdio,
};

/// The authority of the URI the client addresses; it fills `Host`, and the socket path is the
/// address.
const LOCAL_API_MCP_URI_AUTHORITY: &str = "localhost";

/// Bounds an observation verb's tool call: `tap` and `logs` fill a bounded sample runtime-side
/// and can take a moment to.
pub(crate) const OBSERVATION_VERB_TOOL_CALL_TIMEOUT: Duration = Duration::from_secs(30);

/// What an MCP request to a runtime's local API came back with instead of a result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LocalApiMcpToolClientFailureKind {
    /// Nothing answered MCP on the socket, or the answer did not come in time.
    LocalApiUnreachable,
    /// The runtime answered and refused: a JSON-RPC error, no protocol revision in common, or an
    /// answer that was not the request's result.
    RequestRefusedByTheRuntime,
    /// The tool ran and reported a failure, or answered with no text.
    ToolCallFailed,
    /// The connection the client runs over closed: the runtime crashed, was stopped, or dropped
    /// it.
    LocalApiConnectionClosed,
}

/// Why an MCP request to a runtime's local API came back without a result.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{failure_description}")]
pub(crate) struct LocalApiMcpToolClientFailure {
    /// What came back instead of a result.
    pub(crate) kind: LocalApiMcpToolClientFailureKind,
    /// The failure as the verb reports it.
    pub(crate) failure_description: String,
}

impl LocalApiMcpToolClientFailure {
    /// Nothing answered MCP on the socket, or not in time.
    pub(crate) fn local_api_unreachable(failure_description: String) -> Self {
        Self {
            kind: LocalApiMcpToolClientFailureKind::LocalApiUnreachable,
            failure_description,
        }
    }

    /// The runtime answered and refused the request.
    pub(crate) fn request_refused_by_the_runtime(failure_description: String) -> Self {
        Self {
            kind: LocalApiMcpToolClientFailureKind::RequestRefusedByTheRuntime,
            failure_description,
        }
    }

    /// The tool ran and failed, or answered with no text.
    pub(crate) fn tool_call_failed(failure_description: String) -> Self {
        Self {
            kind: LocalApiMcpToolClientFailureKind::ToolCallFailed,
            failure_description,
        }
    }

    /// The connection the client runs over closed.
    pub(crate) fn local_api_connection_closed(failure_description: String) -> Self {
        Self {
            kind: LocalApiMcpToolClientFailureKind::LocalApiConnectionClosed,
            failure_description,
        }
    }
}

impl From<LocalApiMcpToolClientFailure> for TatolabCommandFailure {
    fn from(local_api_mcp_tool_client_failure: LocalApiMcpToolClientFailure) -> Self {
        TatolabCommandFailure::refused(local_api_mcp_tool_client_failure.to_string())
    }
}

/// A connected MCP client of one runtime's local API, driven by the tokio runtime it was connected
/// on.
pub(crate) struct LocalApiMcpToolClient {
    connected_mcp_client: RunningService<RoleClient, ()>,
    local_api_socket_path: PathBuf,
    request_timeout: Duration,
}

impl LocalApiMcpToolClient {
    /// Connect to the local API at `local_api_socket_path` through `server/discover`, bounding
    /// the connect and every later request by `request_timeout`.
    pub(crate) async fn connect(
        local_api_socket_path: &Path,
        request_timeout: Duration,
    ) -> Result<Self, LocalApiMcpToolClientFailure> {
        let local_api_socket_path_text = local_api_socket_path.display().to_string();
        let socket_path_to_dial = socket_path_to_dial(local_api_socket_path).ok_or_else(|| {
            LocalApiMcpToolClientFailure::local_api_unreachable(format!(
                "no runtime answers MCP at {local_api_socket_path_text} (not a socket path this \
                 client can dial)"
            ))
        })?;
        let local_api_mcp_uri =
            format!("http://{LOCAL_API_MCP_URI_AUTHORITY}{MCP_STREAMABLE_HTTP_ROUTE_PATH}");
        // The transport spawns its worker as it is built, so it is built inside the runtime that
        // drives it.
        let transport = StreamableHttpClientTransport::with_client(
            UnixSocketHttpClient::new(&socket_path_to_dial, &local_api_mcp_uri),
            StreamableHttpClientTransportConfig::with_uri(local_api_mcp_uri.as_str()),
        );
        let connected = tokio::time::timeout(
            request_timeout,
            ().serve_with_lifecycle(transport, latest_revision_discover_lifecycle()),
        )
        .await;
        Ok(Self {
            connected_mcp_client: connected_mcp_client_or_failure(
                connected,
                &local_api_socket_path_text,
                request_timeout,
            )?,
            local_api_socket_path: local_api_socket_path.to_path_buf(),
            request_timeout,
        })
    }

    /// Connect to the local API at `local_api_socket_path` over a `/mcp/stdio` upgrade of its
    /// own, through `server/discover`, bounding the upgrade, the connect and every later request
    /// by `request_timeout`. The runtime ties every stream this connection attaches to its life.
    pub(crate) async fn connect_over_an_mcp_stdio_upgrade(
        local_api_socket_path: &Path,
        request_timeout: Duration,
    ) -> Result<Self, LocalApiMcpToolClientFailure> {
        let local_api_socket_path_text = local_api_socket_path.display().to_string();
        let UpgradedLocalApiMcpStdioStream {
            local_api_stream,
            bytes_streamed_behind_the_response_head,
        } = match tokio::time::timeout(
            request_timeout,
            upgrade_local_api_connection_to_mcp_stdio(local_api_socket_path),
        )
        .await
        {
            Ok(Ok(upgraded_mcp_stdio_stream)) => upgraded_mcp_stdio_stream,
            Ok(Err(upgrade_failure)) => {
                return Err(LocalApiMcpToolClientFailure::local_api_unreachable(
                    format!(
                        "the runtime at {local_api_socket_path_text} did not open its MCP \
                         stream: {upgrade_failure}"
                    ),
                ));
            }
            Err(_elapsed) => {
                return Err(LocalApiMcpToolClientFailure::local_api_unreachable(
                    format!(
                        "the runtime at {local_api_socket_path_text} did not open its MCP \
                         stream within {request_timeout:?}"
                    ),
                ));
            }
        };
        let (local_api_stream_read_half, local_api_stream_write_half) =
            local_api_stream.into_split();
        // What hyper read past the `101`'s head comes before anything read from the socket.
        let local_api_stream_reader = std::io::Cursor::new(bytes_streamed_behind_the_response_head)
            .chain(local_api_stream_read_half);
        let connected = tokio::time::timeout(
            request_timeout,
            ().serve_with_lifecycle(
                (local_api_stream_reader, local_api_stream_write_half),
                latest_revision_discover_lifecycle(),
            ),
        )
        .await;
        Ok(Self {
            connected_mcp_client: connected_mcp_client_or_failure(
                connected,
                &local_api_socket_path_text,
                request_timeout,
            )?,
            local_api_socket_path: local_api_socket_path.to_path_buf(),
            request_timeout,
        })
    }

    /// Call `tool_name` with `tool_arguments`, answering the first text block of its result.
    pub(crate) async fn call_tool(
        &self,
        tool_name: &str,
        tool_arguments: serde_json::Map<String, serde_json::Value>,
    ) -> Result<String, LocalApiMcpToolClientFailure> {
        self.call_tool_bounded_by(tool_name, tool_arguments, self.request_timeout)
            .await
    }

    /// [`Self::call_tool`], waiting at most `call_timeout` for the result rather than the
    /// client's request bound.
    pub(crate) async fn call_tool_bounded_by(
        &self,
        tool_name: &str,
        tool_arguments: serde_json::Map<String, serde_json::Value>,
        call_timeout: Duration,
    ) -> Result<String, LocalApiMcpToolClientFailure> {
        let request =
            CallToolRequestParams::new(tool_name.to_owned()).with_arguments(tool_arguments);
        match tokio::time::timeout(call_timeout, self.connected_mcp_client.call_tool(request)).await
        {
            Ok(Ok(tool_result)) => first_text_block_of_tool_result(tool_name, tool_result),
            Ok(Err(service_failure)) => {
                Err(self.failure_for_a_tool_call_without_a_result(tool_name, service_failure))
            }
            Err(_elapsed) => Err(LocalApiMcpToolClientFailure::local_api_unreachable(
                format!(
                    "`{tool_name}` to the runtime at {} did not answer within {call_timeout:?}",
                    self.local_api_socket_path.display(),
                ),
            )),
        }
    }

    /// End the client's connection, waiting at most the request bound.
    pub(crate) async fn close(self) {
        // A close that fails or stalls leaves nothing to undo: the local API keeps no session.
        let _closed_or_abandoned =
            tokio::time::timeout(self.request_timeout, self.connected_mcp_client.cancel()).await;
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
                LocalApiMcpToolClientFailure::request_refused_by_the_runtime(format!(
                    "{} ({})",
                    refusal.message, refusal.code.0
                ))
            }
            ServiceError::UnexpectedResponse => {
                LocalApiMcpToolClientFailure::request_refused_by_the_runtime(format!(
                    "the runtime at {} answered `{tool_name}` with something other than its \
                     result",
                    self.local_api_socket_path.display()
                ))
            }
            closed_connection_failure @ (ServiceError::TransportClosed
            | ServiceError::TransportSend(_)) => {
                LocalApiMcpToolClientFailure::local_api_connection_closed(format!(
                    "`{tool_name}` to the runtime at {} failed: {closed_connection_failure}",
                    self.local_api_socket_path.display()
                ))
            }
            other_failure => LocalApiMcpToolClientFailure::local_api_unreachable(format!(
                "`{tool_name}` to the runtime at {} failed: {other_failure}",
                self.local_api_socket_path.display()
            )),
        }
    }
}

/// The path handed to the socket client: absolute, because `rmcp` reads a leading `@` as a Linux
/// abstract socket and refuses an empty path by panicking, where the local API socket is a file.
fn socket_path_to_dial(local_api_socket_path: &Path) -> Option<String> {
    let absolute_socket_path = std::path::absolute(local_api_socket_path).ok()?;
    absolute_socket_path.to_str().map(str::to_owned)
}

/// `server/discover` at the latest revision, the only one a runtime's local API serves.
fn latest_revision_discover_lifecycle() -> ClientLifecycleMode {
    ClientLifecycleMode::Discover {
        preferred_versions: vec![ProtocolVersion::LATEST],
    }
}

/// The client `connected` holds, or the failure a connect to the runtime at
/// `local_api_socket_path_text` came back with instead.
fn connected_mcp_client_or_failure(
    connected: Result<
        Result<RunningService<RoleClient, ()>, ClientInitializeError>,
        tokio::time::error::Elapsed,
    >,
    local_api_socket_path_text: &str,
    request_timeout: Duration,
) -> Result<RunningService<RoleClient, ()>, LocalApiMcpToolClientFailure> {
    match connected {
        Ok(Ok(connected_mcp_client)) => Ok(connected_mcp_client),
        Ok(Err(ClientInitializeError::JsonRpcError(refusal))) => Err(
            LocalApiMcpToolClientFailure::request_refused_by_the_runtime(format!(
                "the runtime at {local_api_socket_path_text} refused `server/discover`: {} ({})",
                refusal.message, refusal.code.0
            )),
        ),
        Ok(Err(ClientInitializeError::NoCompatibleProtocolVersion {
            server_supported, ..
        })) => Err(
            LocalApiMcpToolClientFailure::request_refused_by_the_runtime(format!(
                "the runtime at {local_api_socket_path_text} serves {server_supported:?}, not {}",
                ProtocolVersion::LATEST
            )),
        ),
        Ok(Err(connect_failure)) => Err(LocalApiMcpToolClientFailure::local_api_unreachable(
            format!("no runtime answers MCP at {local_api_socket_path_text} ({connect_failure})"),
        )),
        Err(_elapsed) => Err(LocalApiMcpToolClientFailure::local_api_unreachable(
            format!(
                "no runtime answered MCP at {local_api_socket_path_text} within {request_timeout:?}"
            ),
        )),
    }
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
        (true, failure_text) => Err(LocalApiMcpToolClientFailure::tool_call_failed(format!(
            "{tool_name} failed: {}",
            failure_text.as_deref().unwrap_or("no detail given")
        ))),
        (false, None) => Err(LocalApiMcpToolClientFailure::tool_call_failed(format!(
            "{tool_name} returned no text content"
        ))),
    }
}

/// Connect, call `tool_name` once with `tool_arguments`, and close: an observation verb's one
/// round trip, each failure worded as the verb reports it.
pub(crate) fn call_one_local_api_tool(
    local_api_socket_path: &Path,
    tool_name: &str,
    tool_arguments: serde_json::Map<String, serde_json::Value>,
) -> Result<String, LocalApiMcpToolClientFailure> {
    let mut local_api_connection =
        LocalApiConnection::open(local_api_socket_path, OBSERVATION_VERB_TOOL_CALL_TIMEOUT)?;
    local_api_connection
        .call_tool(tool_name, tool_arguments)
        .map_err(|call_failure| {
            tool_call_failure_worded_as_an_observation_verb_reports_it(tool_name, call_failure)
        })
}

/// `call_failure` of a connect or call for `tool_name`, worded as an observation verb reports it:
/// a refusal gains the `{tool_name} failed: ` a tool's own failure already carries.
pub(crate) fn tool_call_failure_worded_as_an_observation_verb_reports_it(
    tool_name: &str,
    call_failure: LocalApiMcpToolClientFailure,
) -> LocalApiMcpToolClientFailure {
    if call_failure.kind != LocalApiMcpToolClientFailureKind::RequestRefusedByTheRuntime {
        return call_failure;
    }
    LocalApiMcpToolClientFailure::request_refused_by_the_runtime(format!(
        "{tool_name} failed: {}",
        call_failure.failure_description
    ))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::stub_local_api_server::{
        NOTHING_LISTENS_LOCAL_API_SOCKET_PATH, RecordedToolCall, StubLocalApiScript,
        StubLocalApiServer, StubMcpStdioUpgradeAnswer, StubToolAnswer, StubToolCallTransport,
    };

    fn json_object(json_value: serde_json::Value) -> serde_json::Map<String, serde_json::Value> {
        json_value.as_object().cloned().unwrap()
    }

    /// Run `connected_client_round` against a client connected over its own `/mcp/stdio`
    /// upgrade to `local_api_socket_path`.
    fn over_an_mcp_stdio_connection<RoundOutcome>(
        local_api_socket_path: &Path,
        connected_client_round: impl AsyncFnOnce(LocalApiMcpToolClient) -> RoundOutcome,
    ) -> RoundOutcome {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                let local_api_mcp_tool_client =
                    LocalApiMcpToolClient::connect_over_an_mcp_stdio_upgrade(
                        local_api_socket_path,
                        OBSERVATION_VERB_TOOL_CALL_TIMEOUT,
                    )
                    .await
                    .unwrap();
                connected_client_round(local_api_mcp_tool_client).await
            })
    }

    #[test]
    fn a_socket_path_no_client_can_dial_is_unreachable_rather_than_a_panic() {
        for undialable_socket_path in ["", "@"] {
            let unreachable = call_one_local_api_tool(
                Path::new(undialable_socket_path),
                "graph",
                serde_json::Map::new(),
            )
            .unwrap_err();
            assert_eq!(
                unreachable.kind,
                LocalApiMcpToolClientFailureKind::LocalApiUnreachable,
                "{undialable_socket_path:?}"
            );
        }
    }

    #[test]
    fn a_client_over_an_mcp_stdio_upgrade_calls_tools_on_one_connection() {
        let stub_local_api_server = StubLocalApiServer::serve(StubLocalApiScript {
            queued_tool_answers: vec![
                StubToolAnswer::tool_result("first"),
                StubToolAnswer::tool_result("second"),
            ],
            ..StubLocalApiScript::default()
        });

        let answers = over_an_mcp_stdio_connection(
            &stub_local_api_server.local_api_socket_path,
            async |local_api_mcp_tool_client| {
                let first = local_api_mcp_tool_client
                    .call_tool("run_stream", json_object(json!({"keep": false})))
                    .await
                    .unwrap();
                let second = local_api_mcp_tool_client
                    .call_tool("logs", json_object(json!({"stream": "s", "after": 0})))
                    .await
                    .unwrap();
                local_api_mcp_tool_client.close().await;
                [first, second]
            },
        );

        assert_eq!(answers, ["first", "second"]);
        assert_eq!(
            stub_local_api_server
                .recorded_mcp_stdio_request_heads()
                .len(),
            1,
            "both calls ride the one upgraded connection"
        );
        assert_eq!(
            stub_local_api_server.recorded_tool_calls(),
            [
                RecordedToolCall {
                    tool_name: "run_stream".to_owned(),
                    tool_arguments: json!({"keep": false}),
                    tool_call_transport: StubToolCallTransport::McpStdioConnection,
                },
                RecordedToolCall {
                    tool_name: "logs".to_owned(),
                    tool_arguments: json!({"stream": "s", "after": 0}),
                    tool_call_transport: StubToolCallTransport::McpStdioConnection,
                },
            ]
        );
    }

    #[test]
    fn a_call_after_the_runtime_closed_the_mcp_stdio_connection_reads_as_the_connection_closed() {
        let stub_local_api_server = StubLocalApiServer::serve_default();

        let closed_call_failure = over_an_mcp_stdio_connection(
            &stub_local_api_server.local_api_socket_path,
            async |local_api_mcp_tool_client| {
                local_api_mcp_tool_client
                    .call_tool("list_streams", serde_json::Map::new())
                    .await
                    .unwrap();
                stub_local_api_server.close_every_mcp_stdio_connection();
                let mut closed_call_outcome = local_api_mcp_tool_client
                    .call_tool("list_streams", serde_json::Map::new())
                    .await;
                // The close lands on the stub's own runtime; a call that raced it is retried.
                for _ in 0..50 {
                    if closed_call_outcome.is_err() {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(20)).await;
                    closed_call_outcome = local_api_mcp_tool_client
                        .call_tool("list_streams", serde_json::Map::new())
                        .await;
                }
                closed_call_outcome.unwrap_err()
            },
        );

        assert_eq!(
            closed_call_failure.kind,
            LocalApiMcpToolClientFailureKind::LocalApiConnectionClosed,
            "{closed_call_failure}"
        );
    }

    #[test]
    fn a_runtime_refusing_the_mcp_stdio_upgrade_is_unreachable_naming_its_answer() {
        let stub_local_api_server = StubLocalApiServer::serve_answering_the_mcp_stdio_upgrade_with(
            StubMcpStdioUpgradeAnswer::RefuseTheUpgrade { http_status: 426 },
        );

        let refused = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(LocalApiMcpToolClient::connect_over_an_mcp_stdio_upgrade(
                &stub_local_api_server.local_api_socket_path,
                OBSERVATION_VERB_TOOL_CALL_TIMEOUT,
            ))
            .err()
            .unwrap();

        assert_eq!(
            refused.to_string(),
            format!(
                "the runtime at {} did not open its MCP stream: it answered `HTTP/1.1 426 \
                 Upgrade Required`",
                stub_local_api_server.local_api_socket_path.display()
            )
        );
        assert_eq!(
            refused.kind,
            LocalApiMcpToolClientFailureKind::LocalApiUnreachable
        );
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
                tool_call_transport: StubToolCallTransport::StreamableHttpPost,
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
        let mut local_api_connection = LocalApiConnection::open(
            &stub_local_api_server.local_api_socket_path,
            OBSERVATION_VERB_TOOL_CALL_TIMEOUT,
        )
        .unwrap();

        let answers: Vec<String> = (0..3)
            .map(|_| {
                local_api_connection
                    .call_tool("graph", serde_json::Map::new())
                    .unwrap()
            })
            .collect();
        drop(local_api_connection);

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
            LocalApiMcpToolClientFailure::tool_call_failed(
                "tap failed: no such channel".to_owned()
            )
        );
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
            LocalApiMcpToolClientFailure::request_refused_by_the_runtime(
                "nope failed: no tool named `nope` (-32602)".to_owned()
            )
        );
    }

    #[test]
    fn an_unreachable_local_api_socket_is_named() {
        let unreachable = call_one_local_api_tool(
            Path::new(NOTHING_LISTENS_LOCAL_API_SOCKET_PATH),
            "graph",
            serde_json::Map::new(),
        )
        .unwrap_err();

        assert_eq!(
            unreachable.kind,
            LocalApiMcpToolClientFailureKind::LocalApiUnreachable
        );
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
            Err(LocalApiMcpToolClientFailure::tool_call_failed(
                "graph returned no text content".to_owned()
            ))
        );
        assert_eq!(
            first_text_block_of_tool_result("graph", CallToolResult::error(Vec::new())),
            Err(LocalApiMcpToolClientFailure::tool_call_failed(
                "graph failed: no detail given".to_owned()
            ))
        );
    }
}
