// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `/mcp/stdio`: the node's MCP server over MCP's stdio framing.
//!
//! An HTTP/1.1 request carrying `Upgrade: mcp-stdio` is answered `101`, and the
//! upgraded connection then carries newline-delimited JSON-RPC both ways —
//! the framing the spec names for a byte-stream socket. `rmcp` serves it with
//! the same [`LocalApiMcpServerHandler`] `POST /mcp` serves, so the protocol is
//! `rmcp`'s alone: concurrency, `notifications/cancelled`, the in-flight
//! answers a closing stream still owes, and `subscriptions/listen`.
//!
//! A stream `run_stream` loads over a connection is recorded and outlives it.
//!
//! `tatolab mcp` and `tatolab dev` are the clients: each sends the upgrade.

use axum::extract::Request;
use axum::http::header::{CONNECTION, UPGRADE};
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{MethodRouter, get};
use hyper_util::rt::TokioIo;
use rmcp::ServiceExt;
use rmcp::service::ServerInitializeError;
use streamlib_runtime_client_contract::local_api_wire_contract::{
    MCP_STDIO_UPGRADE_PROTOCOL_TOKEN, MCP_STDIO_UPGRADE_REQUEST_TARGET,
};
use tokio_util::sync::CancellationToken;

use crate::mcp::LocalApiMcpServerHandler;

/// `/mcp/stdio`'s route: each upgraded connection is served until it closes
/// or `local_api_stopping_token` is cancelled.
pub(crate) fn local_api_mcp_stdio_upgrade_route<S>(
    handler: LocalApiMcpServerHandler,
    local_api_stopping_token: CancellationToken,
) -> MethodRouter<S>
where
    S: Clone + Send + Sync + 'static,
{
    get(move |request: Request| {
        answer_the_mcp_stdio_upgrade(handler.clone(), local_api_stopping_token.clone(), request)
    })
}

async fn answer_the_mcp_stdio_upgrade(
    handler: LocalApiMcpServerHandler,
    local_api_stopping_token: CancellationToken,
    mut request: Request,
) -> Response {
    if !requests_the_mcp_stdio_upgrade(request.headers()) {
        return (
            StatusCode::UPGRADE_REQUIRED,
            mcp_stdio_upgrade_response_headers(),
            format!(
                "`{MCP_STDIO_UPGRADE_REQUEST_TARGET}` serves MCP only after `Connection: upgrade` \
                 and `Upgrade: {MCP_STDIO_UPGRADE_PROTOCOL_TOKEN}`"
            ),
        )
            .into_response();
    }

    let pending_upgrade = hyper::upgrade::on(&mut request);
    tokio::spawn(async move {
        match pending_upgrade.await {
            Ok(upgraded) => {
                serve_local_api_mcp_over_the_upgraded_stream(
                    handler,
                    TokioIo::new(upgraded),
                    local_api_stopping_token,
                )
                .await;
            }
            Err(error) => {
                tracing::warn!(%error, "an `mcp-stdio` upgrade was answered but never completed");
            }
        }
    });

    (
        StatusCode::SWITCHING_PROTOCOLS,
        mcp_stdio_upgrade_response_headers(),
    )
        .into_response()
}

/// The headers that name the upgrade, on its `101` and on a `426` refusing a
/// request that did not ask for it.
fn mcp_stdio_upgrade_response_headers() -> [(HeaderName, HeaderValue); 2] {
    [
        (CONNECTION, HeaderValue::from_static("upgrade")),
        (
            UPGRADE,
            HeaderValue::from_static(MCP_STDIO_UPGRADE_PROTOCOL_TOKEN),
        ),
    ]
}

fn requests_the_mcp_stdio_upgrade(request_headers: &HeaderMap) -> bool {
    header_lists_token(request_headers, CONNECTION, "upgrade")
        && header_lists_token(request_headers, UPGRADE, MCP_STDIO_UPGRADE_PROTOCOL_TOKEN)
}

/// Whether any `header_name` value, read as HTTP's comma-separated list, holds
/// `expected_token`, case-insensitively.
fn header_lists_token(
    request_headers: &HeaderMap,
    header_name: HeaderName,
    expected_token: &str,
) -> bool {
    request_headers
        .get_all(header_name)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .any(|token| token.trim().eq_ignore_ascii_case(expected_token))
}

/// Serve one upgraded connection until it closes or the local API stops.
async fn serve_local_api_mcp_over_the_upgraded_stream(
    handler: LocalApiMcpServerHandler,
    upgraded_stream: TokioIo<hyper::upgrade::Upgraded>,
    local_api_stopping_token: CancellationToken,
) {
    let handler_for_this_connection = handler.for_one_mcp_stdio_connection();
    // Until its first request picks a lifecycle `rmcp` reads without a
    // cancellation token, so the local API stopping has to end that wait here.
    let running_service = tokio::select! {
        served = handler_for_this_connection
            .serve_with_ct(upgraded_stream, local_api_stopping_token.child_token()) => Some(served),
        () = local_api_stopping_token.cancelled() => None,
    };
    match running_service {
        Some(Ok(running_service)) => {
            if let Err(error) = running_service.waiting().await {
                tracing::warn!(%error, "an MCP stdio stream's service task failed");
            }
        }
        Some(Err(error)) => {
            tracing::debug!(
                "an MCP stdio stream ended before serving a request: {}",
                why_an_mcp_stdio_stream_ended_before_serving(&error)
            );
        }
        None => {}
    }
}

/// Why `error` ended a stream before it served a request, naming no message:
/// `rmcp`'s own rendering carries the message it received whole.
fn why_an_mcp_stdio_stream_ended_before_serving(error: &ServerInitializeError) -> &'static str {
    match error {
        ServerInitializeError::ExpectedInitializeRequest(_) => {
            "its first message opened no lifecycle"
        }
        ServerInitializeError::ConnectionClosed(_) => "the connection closed",
        ServerInitializeError::UnexpectedInitializeResponse(_) => {
            "it answered with a result the server did not expect"
        }
        ServerInitializeError::InitializeFailed(_) => "its initialize failed",
        ServerInitializeError::TransportError { .. } => "its transport failed",
        ServerInitializeError::Cancelled => "the local API stopped",
        _ => "rmcp refused it",
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use rmcp::RoleClient;
    use rmcp::model::{
        ClientCapabilities, Implementation, ProtocolVersion, ReadResourceRequestParams,
        RequestMetaObject, SubscriptionFilter,
    };
    use rmcp::service::{
        ClientLifecycleMode, ClientServiceExt, RunningService, ServiceExt, SubscriptionEnd,
    };
    use serde_json::{Value, json};
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
    use tokio::net::UnixStream;
    use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};

    use crate::control_plane_stub_support::{
        LocalApiServedOnAFreshSocket, STUB_STREAM_ID, response_head_over_the_socket,
    };
    use crate::mcp::tests::{
        ControlPlaneMcpDispatchStubRuntime, HandshakeRevisionMcpClient,
        assert_names_exactly_the_control_vocabulary,
    };
    use crate::mcp_resources::{LIVE_GRAPH_RESOURCE_URI, RELAY_ADAPTER_CATALOG_RESOURCE_URI};

    /// How long the stub's slow `add_node` takes to answer.
    const SLOW_ADD_NODE_ANSWERS_AFTER: Duration = Duration::from_millis(500);

    /// Longer than the stub's slow `add_node` takes to answer.
    const LONGER_THAN_A_SLOW_ADD_NODE: Duration = Duration::from_millis(1500);

    /// A node whose `add_node` answers only after
    /// [`SLOW_ADD_NODE_ANSWERS_AFTER`], so it is the request still in flight.
    fn served_over_a_slow_add_node() -> LocalApiServedOnAFreshSocket {
        LocalApiServedOnAFreshSocket::over(Arc::new(
            ControlPlaneMcpDispatchStubRuntime::with_add_node_answering_after(
                SLOW_ADD_NODE_ANSWERS_AFTER,
            ),
        ))
    }

    /// The arguments of the slow `add_node` call.
    fn slow_add_node_arguments() -> Value {
        json!({ "stream": STUB_STREAM_ID, "type": "nodes.slow:SlowNode" })
    }

    pub(crate) async fn upgraded_mcp_stdio_stream(
        served: &LocalApiServedOnAFreshSocket,
    ) -> UnixStream {
        let mut stream = UnixStream::connect(&served.local_api_socket_path)
            .await
            .unwrap();
        let head = response_head_over_the_socket(
            &mut stream,
            "GET /mcp/stdio HTTP/1.1\r\nHost: localhost\r\nConnection: Upgrade\r\n\
             Upgrade: mcp-stdio\r\n\r\n",
        )
        .await;
        assert!(head.starts_with("HTTP/1.1 101 "), "{head}");
        assert!(
            head.to_ascii_lowercase().contains("upgrade: mcp-stdio\r\n"),
            "{head}"
        );
        stream
    }

    /// The upgraded stream as newline-delimited JSON-RPC, written and read raw.
    struct McpStdioLines {
        from_the_node: BufReader<OwnedReadHalf>,
        to_the_node: OwnedWriteHalf,
    }

    impl McpStdioLines {
        async fn upgraded_on(served: &LocalApiServedOnAFreshSocket) -> Self {
            let (read_half, write_half) = upgraded_mcp_stdio_stream(served).await.into_split();
            Self {
                from_the_node: BufReader::new(read_half),
                to_the_node: write_half,
            }
        }

        async fn send(&mut self, message: Value) {
            let mut line = serde_json::to_vec(&message).unwrap();
            line.push(b'\n');
            self.to_the_node.write_all(&line).await.unwrap();
        }

        async fn send_request(&mut self, id: u64, method: &str, mut params: Value) {
            params["_meta"] = serde_json::to_value(RequestMetaObject::with_client_context(
                ProtocolVersion::LATEST,
                Implementation::new("mcp-stdio-test", "0"),
                ClientCapabilities::default(),
            ))
            .unwrap();
            self.send(json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }))
                .await;
        }

        async fn send_tool_call(&mut self, id: u64, tool_name: &str, arguments: Value) {
            self.send_request(
                id,
                "tools/call",
                json!({ "name": tool_name, "arguments": arguments }),
            )
            .await;
        }

        /// The next line the node writes, or `None` at end of stream or when
        /// nothing arrives within `wait`.
        async fn next_line_within(&mut self, wait: Duration) -> Option<String> {
            let mut line = String::new();
            match tokio::time::timeout(wait, self.from_the_node.read_line(&mut line)).await {
                Ok(Ok(0)) | Err(_) => None,
                Ok(read) => {
                    read.unwrap();
                    Some(line)
                }
            }
        }

        async fn next_message(&mut self) -> Value {
            let line = self
                .next_line_within(Duration::from_secs(5))
                .await
                .expect("the node writes a message");
            serde_json::from_str(&line).unwrap_or_else(|error| panic!("{error}: {line:?}"))
        }

        async fn reaches_end_of_stream_within(&mut self, wait: Duration) -> bool {
            let mut rest = Vec::new();
            matches!(
                tokio::time::timeout(wait, self.from_the_node.read_to_end(&mut rest)).await,
                Ok(Ok(0))
            )
        }
    }

    /// An `rmcp` client over a fresh `/mcp/stdio` upgrade of `served`.
    pub(crate) async fn rmcp_client_over_the_upgraded_stream(
        served: &LocalApiServedOnAFreshSocket,
    ) -> RunningService<RoleClient, ()> {
        ().serve_with_lifecycle(
            upgraded_mcp_stdio_stream(served).await,
            ClientLifecycleMode::Discover {
                preferred_versions: vec![ProtocolVersion::LATEST],
            },
        )
        .await
        .expect("the node answers `server/discover` over the upgraded stream")
    }

    #[tokio::test]
    async fn a_request_without_the_upgrade_is_answered_upgrade_required_naming_mcp_stdio() {
        let served = served_over_a_slow_add_node();
        let mut stream = UnixStream::connect(&served.local_api_socket_path)
            .await
            .unwrap();

        let head = response_head_over_the_socket(
            &mut stream,
            "GET /mcp/stdio HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
        )
        .await;

        assert!(head.starts_with("HTTP/1.1 426 "), "{head}");
        assert!(
            head.to_ascii_lowercase().contains("upgrade: mcp-stdio\r\n"),
            "{head}"
        );
    }

    #[tokio::test]
    async fn the_upgraded_stream_answers_each_request_with_one_json_rpc_line() {
        let served = served_over_a_slow_add_node();
        let mut lines = McpStdioLines::upgraded_on(&served).await;

        lines.send_request(1, "tools/list", json!({})).await;
        let line = lines
            .next_line_within(Duration::from_secs(5))
            .await
            .expect("the node answers");

        let (message, terminator) = line.split_at(line.len() - 1);
        assert_eq!(terminator, "\n");
        assert!(!message.contains('\n'), "{line:?}");
        let answer: Value = serde_json::from_str(message).unwrap();
        assert_eq!(answer["id"], 1, "{answer}");
        assert!(answer["result"]["tools"].is_array(), "{answer}");
    }

    #[tokio::test]
    async fn the_upgraded_stream_serves_the_same_tools_and_resources_as_post_mcp() {
        let served = served_over_a_slow_add_node();
        let client = rmcp_client_over_the_upgraded_stream(&served).await;

        let tools = client.list_all_tools().await.unwrap();
        assert_names_exactly_the_control_vocabulary(
            tools.iter().map(|tool| tool.name.as_ref()).collect(),
        );

        let live_graph = client
            .read_resource(ReadResourceRequestParams::new(LIVE_GRAPH_RESOURCE_URI))
            .await
            .unwrap();
        assert_eq!(live_graph.contents.len(), 1, "{live_graph:?}");
        let relay_adapter_catalog = client
            .read_resource(ReadResourceRequestParams::new(
                RELAY_ADAPTER_CATALOG_RESOURCE_URI,
            ))
            .await
            .unwrap();
        assert_eq!(
            relay_adapter_catalog.contents.len(),
            1,
            "{relay_adapter_catalog:?}"
        );
    }

    #[tokio::test]
    async fn discover_over_the_upgraded_stream_lists_every_revision_and_a_latest_client_negotiates_the_latest()
     {
        let served = served_over_a_slow_add_node();
        let mut lines = McpStdioLines::upgraded_on(&served).await;

        lines.send_request(1, "server/discover", json!({})).await;
        let discovered = lines.next_message().await;
        assert_eq!(
            discovered["result"]["supportedVersions"],
            json!(ProtocolVersion::KNOWN_VERSIONS),
            "{discovered}"
        );

        let client = rmcp_client_over_the_upgraded_stream(&served).await;
        assert_eq!(
            client
                .peer_info()
                .map(|server_info| server_info.protocol_version.clone()),
            Some(ProtocolVersion::LATEST)
        );
    }

    /// An MCP host that still opens with `initialize` — Claude Desktop among
    /// them — is answered at the revision it named and served the same tools.
    #[tokio::test]
    async fn an_initialize_handshake_is_answered_at_the_revision_it_names_and_serves_the_tools() {
        for handshake_protocol_version in [
            ProtocolVersion::V_2024_11_05,
            ProtocolVersion::V_2025_06_18,
            ProtocolVersion::LATEST_WITH_INITIALIZE,
        ] {
            let served = served_over_a_slow_add_node();
            let client = HandshakeRevisionMcpClient {
                handshake_protocol_version: handshake_protocol_version.clone(),
            }
            .serve_with_lifecycle(
                upgraded_mcp_stdio_stream(&served).await,
                ClientLifecycleMode::Initialize,
            )
            .await
            .unwrap_or_else(|refusal| panic!("{handshake_protocol_version}: {refusal}"));

            assert_eq!(
                client
                    .peer_info()
                    .map(|server_info| server_info.protocol_version.clone()),
                Some(handshake_protocol_version.clone())
            );
            let tools = client.list_all_tools().await.unwrap();
            assert_names_exactly_the_control_vocabulary(
                tools.iter().map(|tool| tool.name.as_ref()).collect(),
            );
            client.cancel().await.unwrap();
        }
    }

    /// A handshake naming a revision the node does not know is answered at the
    /// newest revision that still has a handshake, for the host to accept or
    /// refuse.
    #[tokio::test]
    async fn an_initialize_handshake_naming_an_unknown_revision_is_answered_at_the_newest_with_a_handshake()
     {
        let served = served_over_a_slow_add_node();
        let mut lines = McpStdioLines::upgraded_on(&served).await;

        lines
            .send(json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "protocolVersion": "2024-10-07",
                    "capabilities": {},
                    "clientInfo": { "name": "mcp-stdio-test", "version": "0" },
                },
            }))
            .await;
        let answer = lines.next_message().await;
        assert_eq!(
            answer["result"]["protocolVersion"],
            json!(ProtocolVersion::LATEST_WITH_INITIALIZE),
            "{answer}"
        );

        lines
            .send(json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }))
            .await;
        lines
            .send(json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {} }))
            .await;
        let tools = lines.next_message().await;
        assert!(tools["result"]["tools"].is_array(), "{tools}");
    }

    #[tokio::test]
    async fn requests_run_concurrently_and_each_answer_is_written_as_it_completes() {
        let served = served_over_a_slow_add_node();
        let mut lines = McpStdioLines::upgraded_on(&served).await;

        lines
            .send_tool_call(1, "add_node", slow_add_node_arguments())
            .await;
        lines.send_tool_call(2, "graph", json!({})).await;

        assert_eq!(
            lines.next_message().await["id"],
            2,
            "graph overtakes the slow add_node"
        );
        assert_eq!(lines.next_message().await["id"], 1);
    }

    #[tokio::test]
    async fn a_cancelled_request_is_never_answered() {
        let served = served_over_a_slow_add_node();
        let mut lines = McpStdioLines::upgraded_on(&served).await;

        lines
            .send_tool_call(1, "add_node", slow_add_node_arguments())
            .await;
        lines
            .send(json!({
                "jsonrpc": "2.0",
                "method": "notifications/cancelled",
                "params": { "requestId": 1 },
            }))
            .await;
        lines.send_tool_call(2, "graph", json!({})).await;

        assert_eq!(lines.next_message().await["id"], 2);
        let late = lines.next_line_within(LONGER_THAN_A_SLOW_ADD_NODE).await;
        assert_eq!(late, None, "the cancelled add_node was answered");
        lines.send_tool_call(3, "graph", json!({})).await;
        assert_eq!(
            lines.next_message().await["id"],
            3,
            "the stream outlives the cancel, so the silence was the dropped answer"
        );
    }

    /// `tatolab mcp` half-closes on its stdin's end; what the node already
    /// accepted is still answered before the node closes its side.
    #[tokio::test]
    async fn half_closing_the_stream_answers_the_requests_in_flight_then_closes() {
        let served = served_over_a_slow_add_node();
        let mut lines = McpStdioLines::upgraded_on(&served).await;

        lines
            .send_tool_call(1, "add_node", slow_add_node_arguments())
            .await;
        lines.to_the_node.shutdown().await.unwrap();

        assert_eq!(lines.next_message().await["id"], 1);
        assert!(
            lines
                .reaches_end_of_stream_within(Duration::from_secs(5))
                .await,
            "the node closes its side once nothing is in flight"
        );
    }

    #[tokio::test]
    async fn a_held_listen_ends_with_its_final_result_when_the_local_api_stops() {
        let mut served = served_over_a_slow_add_node();
        let client = rmcp_client_over_the_upgraded_stream(&served).await;
        let mut subscription = client
            .listen(SubscriptionFilter::default())
            .await
            .expect("the node acknowledges a listen");

        served.stop_serving();

        let ended = tokio::time::timeout(Duration::from_secs(5), subscription.next())
            .await
            .expect("the stream ends once the local API stops");
        assert!(matches!(ended, Ok(None)), "{ended:?}");
        assert!(
            matches!(subscription.end(), Some(SubscriptionEnd::Graceful(_))),
            "the stream closes with the listen request's own final result"
        );
        tokio::time::timeout(Duration::from_secs(5), client.waiting())
            .await
            .expect("the node closes the upgraded stream once the local API stops")
            .unwrap();
    }
}
