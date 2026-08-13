// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Model Context Protocol (MCP) veneer over the api-server's control-plane ops.
//!
//! The MCP dispatch is transport-free: [`dispatch_jsonrpc`] answers one parsed
//! JSON-RPC 2.0 message against an `Arc<dyn RuntimeOperations>` and knows
//! nothing about how the bytes arrived. It has exactly one transport: the
//! Streamable-HTTP endpoint (`POST /mcp`, [`mcp_endpoint`]) on the existing axum
//! stack, with its [`crate::auth`] bearer middleware. That endpoint is mounted
//! with the node and shares its lifecycle, so an MCP host reaches StreamLib by
//! pointing at a running node's URL — there is nothing to start and nothing to
//! attach. It exposes the runtime as MCP *tools* so an LLM agent observes the
//! live graph the same way the REST client does.
//!
//! The vocabulary is observation-shaped — graph, tap, logs, shutdown. Live
//! graph mutation is not part of it: code is the source of truth and the edit
//! loop is `dev`, so there is no tool that submits, replaces, connects, or
//! removes.
//!
//! Two of the tools (`tap`, `logs`) front WebSocket *streams* in the REST API.
//! MCP tools are request/response, so each bridges its stream to a **bounded
//! sample** — both by a count AND a monotonic sample window (a quiet channel /
//! idle event stream returns the partial sample rather than blocking the tool
//! call) — and returns the collected sample as the tool result.

use std::sync::Arc;
use std::time::Duration;

use axum::{
    Json,
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use parking_lot::Mutex;
use serde::Deserialize;
use serde_json::{Value, json};
use streamlib::sdk::error::Result;
use streamlib::sdk::pubsub::{
    DEFAULT_SUBSCRIPTION_LIVE_BUDGET, Event, EventListener, PUBSUB, topics,
};
use streamlib::sdk::runtime::RuntimeOperations;

use crate::state::{AppState, RuntimeShutdownRequest};

/// MCP protocol revision this server implements (the date-stamped spec version
/// echoed back on `initialize`). Advertised verbatim; a client that requested a
/// different revision negotiates down to this one.
const MCP_PROTOCOL_VERSION: &str = "2025-06-18";

/// Server identity reported in the `initialize` result's `serverInfo`.
const MCP_SERVER_NAME: &str = "streamlib-api-server";

/// Server version reported in `serverInfo` — the api-server crate version.
const MCP_SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Bounded sample sizes for the streaming-tool → request/response bridge when
/// the caller does not pin its own `count`.
const DEFAULT_TAP_SAMPLE_COUNT: usize = 8;
const DEFAULT_LOGS_SAMPLE_COUNT: usize = 16;

/// Hard ceiling on a requested sample `count`, so a tool call cannot pin an
/// unbounded collection loop.
const MAX_SAMPLE_COUNT: usize = 1024;

/// Per-bag hex-preview cap for the `tap` tool: the full byte length is always
/// reported, but only the first this-many bytes are hex-encoded into the result
/// so a large encoded bag cannot bloat the JSON-RPC payload.
const MAX_TAP_BAG_PREVIEW_BYTES: usize = 4096;

/// Upper bound on how long the `logs` tool waits to fill its sample before
/// returning what it has collected. This is the bounded sample *window* for the
/// otherwise-unbounded event stream; a sparse / idle runtime returns early with
/// fewer events rather than blocking. Monotonic (tokio timer), never wall-clock.
const LOGS_SAMPLE_WINDOW: Duration = Duration::from_millis(500);

/// Upper bound on how long the `tap` tool waits to fill its bag sample before
/// returning what it has collected. The tap forwarder sends nothing on an idle,
/// slow, or paused channel (it idles on `TAP_IDLE_POLL_BACKOFF`), so without
/// this window a request/response tool call would block until `count` bags
/// actually flow. A quiet channel returns the partial sample (0..N bags)
/// instead. Monotonic (tokio timer), never wall-clock; mirrors
/// [`LOGS_SAMPLE_WINDOW`].
const TAP_SAMPLE_WINDOW: Duration = Duration::from_millis(500);

// ============================================================================
// JSON-RPC envelope
// ============================================================================

/// An inbound MCP message. A *request* carries an `id` and expects a paired
/// response; a *notification* (e.g. `notifications/initialized`) omits `id` and
/// is dispatched for effect with no reply (HTTP acks it `202 Accepted`; stdio
/// writes no response line).
#[derive(Deserialize)]
pub(crate) struct JsonRpcRequest {
    #[serde(default)]
    id: Option<Value>,
    method: String,
    #[serde(default)]
    params: Option<Value>,
}

/// A JSON-RPC error (method-not-found / invalid-params). Tool-execution
/// failures are NOT these — they surface as a successful `tools/call` result
/// with `isError: true`, per the MCP tool-error convention.
struct RpcError {
    code: i64,
    message: String,
}

impl RpcError {
    fn method_not_found(method: &str) -> Self {
        Self {
            code: -32601,
            message: format!("method not found: {method}"),
        }
    }
    fn invalid_params(message: impl Into<String>) -> Self {
        Self {
            code: -32602,
            message: message.into(),
        }
    }
}

/// `POST /mcp` — the MCP Streamable-HTTP endpoint. Dispatches one JSON-RPC
/// message through the transport-free [`dispatch_jsonrpc`] and answers with a
/// single `application/json` response (this server's tools are all
/// request/response, so it never opens an SSE stream); a notification is acked
/// `202 Accepted` with no body.
#[tracing::instrument(skip_all, fields(mcp_method = %request.method))]
pub(crate) async fn mcp_endpoint(
    State(state): State<AppState>,
    Json(request): Json<JsonRpcRequest>,
) -> Response {
    match dispatch_jsonrpc(&state.runtime, &request).await {
        Some(response) => Json(response).into_response(),
        None => StatusCode::ACCEPTED.into_response(),
    }
}

/// Dispatch one parsed MCP JSON-RPC 2.0 message against `runtime`, transport-free.
///
/// Returns the full JSON-RPC response envelope (`result` or `error`) for a
/// request, or `None` for a notification (no `id`) — the caller decides how a
/// no-reply is framed on its transport (HTTP: `202`; stdio: no output line).
/// This is the single MCP surface both the HTTP endpoint and the stdio server
/// call, so the two transports can never diverge.
#[tracing::instrument(skip_all, fields(mcp_method = %request.method))]
pub(crate) async fn dispatch_jsonrpc(
    runtime: &Arc<dyn RuntimeOperations>,
    request: &JsonRpcRequest,
) -> Option<Value> {
    let id = request.id.clone()?;
    let params = request.params.clone().unwrap_or(Value::Null);
    let envelope = match dispatch(runtime, &request.method, params).await {
        Ok(result) => json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": result,
        }),
        Err(error) => json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": { "code": error.code, "message": error.message },
        }),
    };
    Some(envelope)
}

async fn dispatch(
    runtime: &Arc<dyn RuntimeOperations>,
    method: &str,
    params: Value,
) -> std::result::Result<Value, RpcError> {
    match method {
        "initialize" => Ok(initialize_result()),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({ "tools": tool_definitions() })),
        "tools/call" => tools_call(runtime, params).await,
        other => Err(RpcError::method_not_found(other)),
    }
}

fn initialize_result() -> Value {
    json!({
        "protocolVersion": MCP_PROTOCOL_VERSION,
        "capabilities": { "tools": { "listChanged": false } },
        "serverInfo": { "name": MCP_SERVER_NAME, "version": MCP_SERVER_VERSION },
        "instructions": "StreamLib runtime control plane. Tools observe a running node: its processor graph, its channels, and its event stream. The graph is defined by the node's code, not by this surface — there is no tool that mutates it.",
    })
}

// ============================================================================
// Tool catalog
// ============================================================================

/// The MCP tool catalog returned by `tools/list`. Each entry mirrors an
/// api-server control-plane op; the `inputSchema` is the JSON Schema a client
/// validates its `arguments` against.
fn tool_definitions() -> Vec<Value> {
    vec![
        json!({
            "name": "graph",
            "description": "Export the current runtime graph (processors, links, states, metrics) as JSON.",
            "inputSchema": { "type": "object", "properties": {}, "additionalProperties": false },
        }),
        json!({
            "name": "tap",
            "description": "Attach a read-only tap to a channel and collect a bounded sample of raw bags (FrameHeader-framed bytes; a hex preview plus byte length per bag).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "channel": { "type": "string", "description": "Channel data-service name, e.g. {source_processor}/{source_output_port}." },
                    "count": { "type": "integer", "minimum": 1, "description": "Number of bags to collect before returning. Defaults to a small sample." }
                },
                "required": ["channel"],
                "additionalProperties": false
            },
        }),
        json!({
            "name": "logs",
            "description": "Collect a bounded sample of the runtime event stream (all topics) within a short monotonic window.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "count": { "type": "integer", "minimum": 1, "description": "Max events to collect before returning. Defaults to a small sample." }
                },
                "additionalProperties": false
            },
        }),
        json!({
            "name": "shutdown",
            "description": "Ask the runtime to shut down. This is a request observed by whoever owns the run loop, which then runs a normal teardown — not an immediate kill. Idempotent: requesting twice is not an error. Returns as soon as the request is accepted; teardown is not awaited.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "reason": { "type": "string", "description": "Human-readable attribution logged with the request. Omit for unspecified." }
                },
                "additionalProperties": false
            },
        }),
    ]
}

// ============================================================================
// tools/call dispatch
// ============================================================================

async fn tools_call(
    runtime: &Arc<dyn RuntimeOperations>,
    params: Value,
) -> std::result::Result<Value, RpcError> {
    #[derive(Deserialize)]
    struct ToolCallParams {
        name: String,
        #[serde(default)]
        arguments: Value,
    }
    let ToolCallParams { name, arguments } = serde_json::from_value(params)
        .map_err(|e| RpcError::invalid_params(format!("malformed tools/call params: {e}")))?;
    let arguments = if arguments.is_null() {
        json!({})
    } else {
        arguments
    };

    let result = match name.as_str() {
        "graph" => call_graph(runtime).await,
        "tap" => call_tap(runtime, arguments).await,
        "logs" => call_logs(runtime, arguments).await,
        "shutdown" => call_shutdown(runtime, arguments),
        other => tool_error(format!("unknown tool: {other}")),
    };
    Ok(result)
}

async fn call_graph(runtime: &Arc<dyn RuntimeOperations>) -> Value {
    match runtime.to_json_async().await {
        Ok(graph) => tool_ok(graph),
        Err(e) => tool_error(format!("graph export failed: {e}")),
    }
}

async fn call_tap(runtime: &Arc<dyn RuntimeOperations>, arguments: Value) -> Value {
    #[derive(Deserialize)]
    struct TapArgs {
        channel: String,
        #[serde(default)]
        count: Option<usize>,
    }
    let TapArgs { channel, count } = match serde_json::from_value(arguments) {
        Ok(args) => args,
        Err(e) => return tool_error(format!("tap arguments: {e}")),
    };
    let sample = bounded_sample_count(count, DEFAULT_TAP_SAMPLE_COUNT);

    let mut subscription = match runtime.tap_async(channel.clone(), Some(sample)).await {
        Ok(subscription) => subscription,
        Err(e) => return tool_error(format!("tap attach failed: {e}")),
    };

    let mut bags: Vec<Value> = Vec::with_capacity(sample);
    let deadline = tokio::time::Instant::now() + TAP_SAMPLE_WINDOW;
    while bags.len() < sample {
        match tokio::time::timeout_at(deadline, subscription.recv()).await {
            Ok(Some(bytes)) => bags.push(tap_bag_json(&bytes)),
            // Tap exhausted (count reached / forwarder ended), or the bounded
            // sample window elapsed on a quiet channel — return the partial sample.
            Ok(None) | Err(_) => break,
        }
    }
    let dropped_bags = subscription.dropped_bags();

    // `TapSubscription::drop` joins the forwarder OS thread; a synchronous join
    // must never run on a tokio worker, so detach it off the async runtime.
    if let Err(join_error) = tokio::task::spawn_blocking(move || drop(subscription)).await {
        tracing::warn!(channel = %channel, "tap detach task failed to join: {join_error}");
    }

    tool_ok(json!({
        "channel": channel,
        "requested": sample,
        "received": bags.len(),
        "window_ms": TAP_SAMPLE_WINDOW.as_millis(),
        "dropped_bags": dropped_bags,
        "bags": bags,
    }))
}

async fn call_logs(runtime: &Arc<dyn RuntimeOperations>, arguments: Value) -> Value {
    let _ = runtime;
    #[derive(Deserialize)]
    struct LogsArgs {
        #[serde(default)]
        count: Option<usize>,
    }
    let LogsArgs { count } = match serde_json::from_value(arguments) {
        Ok(args) => args,
        Err(e) => return tool_error(format!("logs arguments: {e}")),
    };
    let sample = bounded_sample_count(count, DEFAULT_LOGS_SAMPLE_COUNT);

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Event>();
    let listener = Arc::new(Mutex::new(McpEventForwarder { tx }));
    let subscription_live_signal = PUBSUB.subscribe(topics::ALL, listener.clone());

    // The sample window starts once the subscription can actually receive, so
    // the window this tool reports back is the window it actually sampled.
    if let Err(e) = subscription_live_signal
        .wait_until_subscription_is_live_async(DEFAULT_SUBSCRIPTION_LIVE_BUDGET)
        .await
    {
        return tool_error(format!("event subscription never went live: {e}"));
    }

    let mut events: Vec<Value> = Vec::with_capacity(sample);
    let deadline = tokio::time::Instant::now() + LOGS_SAMPLE_WINDOW;
    while events.len() < sample {
        match tokio::time::timeout_at(deadline, rx.recv()).await {
            Ok(Some(event)) => events.push(event_json(&event)),
            // Forwarder channel closed, or the bounded sample window elapsed.
            Ok(None) | Err(_) => break,
        }
    }
    drop(listener); // Weak-ref cleanup on the next publish.

    tool_ok(json!({
        "requested": sample,
        "received": events.len(),
        "window_ms": LOGS_SAMPLE_WINDOW.as_millis(),
        "events": events,
    }))
}

/// Sync, unlike every other tool call: `request_runtime_shutdown` is
/// fire-and-forget with no completion payload, so there is nothing to await
/// and nothing to block on.
fn call_shutdown(runtime: &Arc<dyn RuntimeOperations>, arguments: Value) -> Value {
    let request: RuntimeShutdownRequest = match serde_json::from_value(arguments) {
        Ok(request) => request,
        Err(e) => return tool_error(format!("shutdown arguments: {e}")),
    };
    let reason = request.reason.unwrap_or_default();

    match runtime.request_runtime_shutdown(&reason) {
        Ok(()) => tool_ok(json!({
            "status": crate::state::RUNTIME_SHUTDOWN_REQUESTED_STATUS,
            "reason": reason,
        })),
        Err(e) => tool_error(format!("shutdown request failed: {e}")),
    }
}

// ============================================================================
// Result shaping
// ============================================================================

/// A successful `tools/call` result: the value rendered as a pretty-JSON text
/// content block (the universally-supported MCP tool-result form).
fn tool_ok(value: Value) -> Value {
    let text = serde_json::to_string_pretty(&value).unwrap_or_else(|_| value.to_string());
    json!({
        "content": [{ "type": "text", "text": text }],
        "isError": false,
    })
}

/// A failed `tools/call` result: an `isError` text block. Tool failures are
/// surfaced this way (not as a JSON-RPC error) so the calling agent sees the
/// message in-band and can react.
fn tool_error(message: impl Into<String>) -> Value {
    json!({
        "content": [{ "type": "text", "text": message.into() }],
        "isError": true,
    })
}

/// Clamp a requested sample count into `[1, MAX_SAMPLE_COUNT]`, defaulting when
/// the caller left it unset.
fn bounded_sample_count(requested: Option<usize>, default: usize) -> usize {
    requested.unwrap_or(default).clamp(1, MAX_SAMPLE_COUNT)
}

/// Render one raw tap bag as JSON: full byte length plus a bounded hex preview
/// (raw bags are wire-neutral bytes; decoding is the caller's concern).
fn tap_bag_json(bytes: &[u8]) -> Value {
    let preview_len = bytes.len().min(MAX_TAP_BAG_PREVIEW_BYTES);
    json!({
        "byte_len": bytes.len(),
        "hex_preview": hex_encode(&bytes[..preview_len]),
        "hex_truncated": preview_len < bytes.len(),
    })
}

fn event_json(event: &Event) -> Value {
    json!({
        "topic": event.topic(),
        "name": event.log_name(),
        "event": serde_json::to_value(event).unwrap_or(Value::Null),
    })
}

fn hex_encode(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut hex = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

/// Forwards runtime events into the `logs` tool's bounded collection channel,
/// mirroring the REST WebSocket event forwarder.
struct McpEventForwarder {
    tx: tokio::sync::mpsc::UnboundedSender<Event>,
}

impl EventListener for McpEventForwarder {
    fn on_event(&mut self, event: &Event) -> Result<()> {
        let _ = self.tx.send(event.clone());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    //! MCP-veneer wire tests: drive the real `POST /mcp` endpoint that
    //! [`crate::handlers::build_router`] wires in, exercising the JSON-RPC
    //! handshake, the tool catalog, and each observation tool through to the
    //! runtime. The router is the real one; only the `RuntimeOperations`
    //! backend is a stub, so the MCP → runtime seam is what's under test.
    //!
    //! The catalog assertions are two-sided on purpose — what is advertised,
    //! and what must never be again.

    use axum::Router;
    use axum::body::Body;
    use axum::http::{Request, StatusCode, header::CONTENT_TYPE};
    use streamlib::sdk::error::Error;
    use streamlib::sdk::runtime::{BoxFuture, RuntimeOperations, TapSubscription};
    use tower::ServiceExt;

    use super::*;

    /// How the stub's `tap_async` answers: either it refuses (no channel), or it
    /// hands back a synthetic [`TapSubscription`] pre-loaded with `bags` and a
    /// fixed `dropped_bags` count. `keep_sender_open` retains the forward
    /// sender so `recv()` pends after the bags drain — modelling a quiet channel
    /// so the tap tool's monotonic sample window is what ends the collection.
    #[derive(Clone)]
    struct StubTapPlan {
        bags: Vec<Vec<u8>>,
        dropped_bags: u64,
        keep_sender_open: bool,
        sender_keepalive: Arc<Mutex<Option<tokio::sync::mpsc::Sender<Vec<u8>>>>>,
    }

    /// Stub runtime answering the observation ops without a live engine, and
    /// recording every shutdown reason so a dispatch test can confirm the tool
    /// reached the matching runtime op.
    ///
    /// Every graph-mutating op is `unreachable!`. `RuntimeOperations` still
    /// declares them — the runtime API is not what changed — but no tool may
    /// reach one, so a dispatch arm that regrows here fails loudly instead of
    /// quietly succeeding against a permissive stub.
    struct ControlPlaneMcpDispatchStubRuntime {
        tap_plan: Option<StubTapPlan>,
        recorded_shutdown_reasons: Arc<Mutex<Vec<String>>>,
    }

    impl ControlPlaneMcpDispatchStubRuntime {
        fn new() -> Self {
            Self {
                tap_plan: None,
                recorded_shutdown_reasons: Arc::new(Mutex::new(Vec::new())),
            }
        }

        /// A stub whose `tap_async` yields a synthetic subscription over `bags`
        /// with the given dropped-bag count, dropping the forward sender once the
        /// bags are queued so `recv()` ends (exhaustion path).
        fn with_tap_bags(bags: Vec<Vec<u8>>, dropped_bags: u64) -> Self {
            Self {
                tap_plan: Some(StubTapPlan {
                    bags,
                    dropped_bags,
                    keep_sender_open: false,
                    sender_keepalive: Arc::new(Mutex::new(None)),
                }),
                ..Self::new()
            }
        }

        /// A stub whose `tap_async` yields a subscription over `bags` but keeps
        /// the forward sender alive, so `recv()` pends after the bags drain — a
        /// quiet channel whose collection ends on the monotonic sample window.
        fn with_quiet_tap(bags: Vec<Vec<u8>>) -> Self {
            Self {
                tap_plan: Some(StubTapPlan {
                    bags,
                    dropped_bags: 0,
                    keep_sender_open: true,
                    sender_keepalive: Arc::new(Mutex::new(None)),
                }),
                ..Self::new()
            }
        }
    }

    impl RuntimeOperations for ControlPlaneMcpDispatchStubRuntime {
        fn to_json_async(&self) -> BoxFuture<'_, Result<Value>> {
            Box::pin(async { Ok(json!({ "processors": [], "links": [] })) })
        }
        fn tap_async(
            &self,
            channel: String,
            _count: Option<usize>,
        ) -> BoxFuture<'_, Result<TapSubscription>> {
            let Some(plan) = self.tap_plan.clone() else {
                return Box::pin(async move { Err(Error::TapChannelNotFound(channel)) });
            };
            Box::pin(async move {
                let (sender, receiver) =
                    tokio::sync::mpsc::channel::<Vec<u8>>(plan.bags.len().max(1));
                for bag in &plan.bags {
                    sender.send(bag.clone()).await.expect("stub tap queue send");
                }
                if plan.keep_sender_open {
                    *plan.sender_keepalive.lock() = Some(sender);
                }
                Ok(TapSubscription::from_forward_channel(
                    channel,
                    receiver,
                    plan.dropped_bags,
                ))
            })
        }
        crate::control_plane_stub_support::graph_mutation_ops_are_unreachable!("tool");
        fn request_runtime_shutdown(&self, reason: &str) -> Result<()> {
            self.recorded_shutdown_reasons
                .lock()
                .push(reason.to_string());
            Ok(())
        }
        fn to_json(&self) -> Result<Value> {
            Ok(json!({}))
        }
    }

    /// The control vocabulary, in catalog order. This is the whole of it —
    /// `tools/list` is asserted equal to this, not merely a superset.
    const OBSERVATION_TOOL_NAMES: &[&str] = &["graph", "tap", "logs", "shutdown"];

    /// The tools this control plane deliberately does not serve: every graph
    /// mutation the pre-pivot MCP veneer exposed.
    const DELETED_GRAPH_MUTATION_TOOL_NAMES: &[&str] = &[
        "submit_processor",
        "replace_processor",
        "remove_processor",
        "connect",
    ];

    fn mcp_router(runtime: Arc<dyn RuntimeOperations>) -> Router {
        crate::handlers::build_router(
            runtime,
            None,
            #[cfg(feature = "moq")]
            "test-runtime-id".to_string(),
        )
    }

    /// POST one JSON-RPC message to `/mcp` and return the parsed JSON body (or
    /// `Value::Null` for an empty `202` notification ack) with the status.
    async fn mcp_call(runtime: Arc<dyn RuntimeOperations>, message: Value) -> (StatusCode, Value) {
        let request = Request::builder()
            .method("POST")
            .uri("/mcp")
            .header(CONTENT_TYPE, "application/json")
            .body(Body::from(message.to_string()))
            .unwrap();
        let response = mcp_router(runtime).oneshot(request).await.unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap()
        };
        (status, body)
    }

    #[tokio::test]
    async fn initialize_handshake_reports_tools_capability() {
        let (status, body) = mcp_call(
            Arc::new(ControlPlaneMcpDispatchStubRuntime::new()),
            json!({
                "jsonrpc": "2.0", "id": 1, "method": "initialize",
                "params": { "protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": { "name": "test", "version": "0" } }
            }),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["id"], 1);
        assert_eq!(body["result"]["protocolVersion"], "2025-06-18");
        assert_eq!(body["result"]["serverInfo"]["name"], "streamlib-api-server");
        assert!(
            body["result"]["capabilities"]["tools"].is_object(),
            "server must advertise the tools capability"
        );
    }

    #[tokio::test]
    async fn notifications_are_acked_with_202_and_no_body() {
        let (status, body) = mcp_call(
            Arc::new(ControlPlaneMcpDispatchStubRuntime::new()),
            json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
        )
        .await;
        assert_eq!(status, StatusCode::ACCEPTED);
        assert_eq!(body, Value::Null);
    }

    #[tokio::test]
    async fn tools_list_advertises_exactly_the_observation_vocabulary() {
        let (status, body) = mcp_call(
            Arc::new(ControlPlaneMcpDispatchStubRuntime::new()),
            json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        let tools = body["result"]["tools"].as_array().expect("tools array");
        let names: Vec<&str> = tools
            .iter()
            .filter_map(|tool| tool["name"].as_str())
            .collect();

        // Exact, not a superset: the catalog IS the control vocabulary, so a
        // tool appearing here that is not in this list is a surface the plan
        // does not grant.
        assert_eq!(
            names, OBSERVATION_TOOL_NAMES,
            "tools/list must advertise exactly the observation vocabulary"
        );
        for tool in tools {
            assert_eq!(
                tool["inputSchema"]["type"], "object",
                "tool `{}` must declare an object inputSchema",
                tool["name"]
            );
        }
    }

    /// Every deleted tool must answer as an unknown tool rather than reaching
    /// the runtime — the dispatch arm is gone, not merely undocumented.
    #[tokio::test]
    async fn tools_call_rejects_every_graph_mutation_tool_as_unknown() {
        for absent in DELETED_GRAPH_MUTATION_TOOL_NAMES {
            let (status, body) = mcp_call(
                Arc::new(ControlPlaneMcpDispatchStubRuntime::new()),
                json!({
                    "jsonrpc": "2.0", "id": 2, "method": "tools/call",
                    "params": { "name": absent, "arguments": {} }
                }),
            )
            .await;

            assert_eq!(status, StatusCode::OK);
            assert_eq!(
                body["result"]["isError"], true,
                "`{absent}` must be an in-band tool error; got {body}"
            );
            let text = body["result"]["content"][0]["text"]
                .as_str()
                .expect("text content block");
            assert!(
                text.contains("unknown tool"),
                "`{absent}` must be rejected as an unknown tool; got {text}"
            );
        }
    }

    #[tokio::test]
    async fn tools_call_graph_returns_the_runtime_json() {
        let (status, body) = mcp_call(
            Arc::new(ControlPlaneMcpDispatchStubRuntime::new()),
            json!({ "jsonrpc": "2.0", "id": 4, "method": "tools/call", "params": { "name": "graph", "arguments": {} } }),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["result"]["isError"], false);
        let text = body["result"]["content"][0]["text"].as_str().unwrap();
        let graph: Value = serde_json::from_str(text).unwrap();
        assert!(graph["processors"].is_array());
    }

    #[tokio::test]
    async fn tools_call_unknown_tool_is_an_in_band_tool_error() {
        let (status, body) = mcp_call(
            Arc::new(ControlPlaneMcpDispatchStubRuntime::new()),
            json!({ "jsonrpc": "2.0", "id": 5, "method": "tools/call", "params": { "name": "does_not_exist", "arguments": {} } }),
        )
        .await;

        // A missing TOOL is an isError result, not a JSON-RPC error — the call
        // itself succeeded.
        assert_eq!(status, StatusCode::OK);
        assert!(body["error"].is_null());
        assert_eq!(body["result"]["isError"], true);
    }

    #[tokio::test]
    async fn mcp_endpoint_is_gated_by_bearer_auth_when_enabled() {
        use axum::http::header::AUTHORIZATION;
        const TOKEN: &str = "mcp-test-secret";

        let auth_router = || {
            crate::handlers::build_router(
                Arc::new(ControlPlaneMcpDispatchStubRuntime::new()),
                Some(crate::auth::ApiServerBearerToken::from_secret(TOKEN)),
                #[cfg(feature = "moq")]
                "test-runtime-id".to_string(),
            )
        };
        let message = json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" }).to_string();

        // No bearer token → the gate rejects with 401 before the JSON-RPC
        // handler runs. Deleting the mcp_router `.route_layer(...)`
        // flips this to 200, going red here.
        let unauthenticated = Request::builder()
            .method("POST")
            .uri("/mcp")
            .header(CONTENT_TYPE, "application/json")
            .body(Body::from(message.clone()))
            .unwrap();
        let status = auth_router()
            .oneshot(unauthenticated)
            .await
            .unwrap()
            .status();
        assert_eq!(status, StatusCode::UNAUTHORIZED);

        // A valid token clears the gate and reaches the handler.
        let authenticated = Request::builder()
            .method("POST")
            .uri("/mcp")
            .header(CONTENT_TYPE, "application/json")
            .header(AUTHORIZATION, format!("Bearer {TOKEN}"))
            .body(Body::from(message))
            .unwrap();
        let status = auth_router().oneshot(authenticated).await.unwrap().status();
        assert_eq!(status, StatusCode::OK);
    }

    #[tokio::test]
    async fn unknown_jsonrpc_method_is_a_method_not_found_error() {
        let (status, body) = mcp_call(
            Arc::new(ControlPlaneMcpDispatchStubRuntime::new()),
            json!({ "jsonrpc": "2.0", "id": 6, "method": "no_such_method" }),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["error"]["code"], -32601);
    }

    #[tokio::test]
    async fn tools_call_tap_shapes_bags_and_reports_dropped_count() {
        let big_bag = vec![0xABu8; MAX_TAP_BAG_PREVIEW_BYTES + 512];
        let small_bag = vec![0x01u8, 0x02, 0x03];
        let runtime = Arc::new(ControlPlaneMcpDispatchStubRuntime::with_tap_bags(
            vec![big_bag.clone(), small_bag.clone()],
            7,
        ));

        let (status, body) = mcp_call(
            runtime,
            json!({
                "jsonrpc": "2.0", "id": 10, "method": "tools/call",
                "params": { "name": "tap", "arguments": { "channel": "cam/frame" } }
            }),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        let result = &body["result"];
        assert_eq!(result["isError"], false, "body={body}");
        let text = result["content"][0]["text"].as_str().unwrap();
        let sample: Value = serde_json::from_str(text).expect("tap result text is JSON");

        assert_eq!(sample["channel"], "cam/frame");
        assert_eq!(sample["received"], 2);
        assert_eq!(sample["dropped_bags"], 7);
        assert!(sample["window_ms"].as_u64().unwrap() > 0);

        let bags = sample["bags"].as_array().expect("bags array");
        // Big bag: the full byte length is reported, the hex preview is capped at
        // MAX_TAP_BAG_PREVIEW_BYTES, and truncation is flagged.
        assert_eq!(
            bags[0]["byte_len"].as_u64().unwrap(),
            (MAX_TAP_BAG_PREVIEW_BYTES + 512) as u64
        );
        assert_eq!(bags[0]["hex_truncated"], true);
        assert_eq!(
            bags[0]["hex_preview"].as_str().unwrap().len(),
            MAX_TAP_BAG_PREVIEW_BYTES * 2,
            "preview is the hex of exactly the first MAX_TAP_BAG_PREVIEW_BYTES bytes"
        );
        // Small bag: previewed whole, not truncated.
        assert_eq!(bags[1]["byte_len"].as_u64().unwrap(), 3);
        assert_eq!(bags[1]["hex_truncated"], false);
        assert_eq!(bags[1]["hex_preview"], "010203");
    }

    #[tokio::test]
    async fn tools_call_tap_returns_partial_sample_within_window_on_quiet_channel() {
        // One bag flows, then the channel goes quiet (the forward sender is kept
        // open) so `recv()` pends; the request asks for four. Without the
        // monotonic sample window this tool call would block until three more
        // bags arrive — the hang this fix closes.
        let runtime = Arc::new(ControlPlaneMcpDispatchStubRuntime::with_quiet_tap(vec![
            vec![0xAA, 0xBB],
        ]));

        let started = tokio::time::Instant::now();
        let (status, body) = mcp_call(
            runtime,
            json!({
                "jsonrpc": "2.0", "id": 11, "method": "tools/call",
                "params": { "name": "tap", "arguments": { "channel": "cam/frame", "count": 4 } }
            }),
        )
        .await;
        let elapsed = started.elapsed();

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["result"]["isError"], false, "body={body}");
        let text = body["result"]["content"][0]["text"].as_str().unwrap();
        let sample: Value = serde_json::from_str(text).unwrap();
        assert_eq!(sample["requested"], 4);
        assert_eq!(
            sample["received"], 1,
            "a quiet channel returns the partial sample, not a full four"
        );
        assert!(
            elapsed < TAP_SAMPLE_WINDOW * 4,
            "tap must return within its sample window, not hang; took {elapsed:?}"
        );
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn tools_call_logs_returns_bounded_window_sample() {
        // A live bus that nobody publishes to: the tool waits for its
        // subscription to go live, then collects nothing, and the monotonic
        // sample window bounds the wait rather than letting it hang. Live event
        // delivery rides iceoryx2 and is exercised by the engine's pubsub
        // integration tests, not here.
        //
        // The bus must be live for the empty sample to mean "the node was
        // quiet" — against an absent bus the tool reports an error instead, so
        // the two would be indistinguishable. `#[serial]` keeps another test's
        // publish out of this window.
        crate::control_plane_stub_support::initialize_process_global_pubsub_for_tests();

        let started = tokio::time::Instant::now();
        let (status, body) = mcp_call(
            Arc::new(ControlPlaneMcpDispatchStubRuntime::new()),
            json!({
                "jsonrpc": "2.0", "id": 12, "method": "tools/call",
                "params": { "name": "logs", "arguments": { "count": 4 } }
            }),
        )
        .await;
        let elapsed = started.elapsed();

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["result"]["isError"], false, "body={body}");
        let text = body["result"]["content"][0]["text"].as_str().unwrap();
        let sample: Value = serde_json::from_str(text).unwrap();
        assert_eq!(sample["requested"], 4);
        assert_eq!(sample["received"], 0);
        assert_eq!(
            sample["window_ms"].as_u64().unwrap(),
            LOGS_SAMPLE_WINDOW.as_millis() as u64
        );
        // The budget names the subscription wait because the measured span now
        // contains it: the tool waits for its subscription before the window
        // starts, so a slow iceoryx2 open is time this assertion must allow
        // rather than a hang it should catch.
        assert!(
            elapsed < LOGS_SAMPLE_WINDOW * 4 + DEFAULT_SUBSCRIPTION_LIVE_BUDGET,
            "logs must return within its sample window, not hang; took {elapsed:?}"
        );
    }

    #[tokio::test]
    async fn tools_call_shutdown_reaches_the_runtime() {
        let runtime = Arc::new(ControlPlaneMcpDispatchStubRuntime::new());
        let recorded_shutdowns = runtime.recorded_shutdown_reasons.clone();

        let (status, body) = mcp_call(
            runtime,
            json!({
                "jsonrpc": "2.0", "id": 16, "method": "tools/call",
                "params": { "name": "shutdown", "arguments": { "reason": "agent asked" } }
            }),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["result"]["isError"], false, "body={body}");
        let text = body["result"]["content"][0]["text"].as_str().unwrap();
        let outcome: Value = serde_json::from_str(text).unwrap();
        assert_eq!(outcome["status"], "RuntimeShutdownRequested");
        assert_eq!(outcome["reason"], "agent asked");
        assert_eq!(
            *recorded_shutdowns.lock(),
            vec!["agent asked".to_string()],
            "the tool must reach `request_runtime_shutdown` with the caller's reason"
        );
    }

    /// A malformed `shutdown` argument is an in-band tool error (`isError`),
    /// never a JSON-RPC error and never a silent shutdown — the agent has to
    /// see why its call did nothing.
    #[tokio::test]
    async fn tools_call_shutdown_with_malformed_arguments_is_an_in_band_tool_error() {
        let runtime = Arc::new(ControlPlaneMcpDispatchStubRuntime::new());
        let recorded_shutdowns = runtime.recorded_shutdown_reasons.clone();

        let (status, body) = mcp_call(
            runtime,
            json!({
                "jsonrpc": "2.0", "id": 17, "method": "tools/call",
                "params": { "name": "shutdown", "arguments": { "reason": 42 } }
            }),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert!(body.get("error").is_none(), "not a JSON-RPC error: {body}");
        assert_eq!(body["result"]["isError"], true, "body={body}");
        assert!(
            body["result"]["content"][0]["text"]
                .as_str()
                .unwrap_or_default()
                .contains("shutdown arguments"),
            "the tool error must name the offending argument set: {body}"
        );
        assert!(
            recorded_shutdowns.lock().is_empty(),
            "a malformed call must not reach the runtime"
        );
    }
}
