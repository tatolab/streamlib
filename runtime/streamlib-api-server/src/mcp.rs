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
//! The vocabulary is observation-shaped — graph, tap, logs, exchange,
//! shutdown. Live graph mutation is not part of it: code is the source of
//! truth and the edit loop is `dev`, so there is no tool that submits,
//! replaces, connects, or removes.
//!
//! `exchange` is the one tool whose result is not text: it answers a
//! published surface id with the frame itself, as an image content block the
//! host renders in-session — so an agent on another machine sees the pixels
//! with no shared filesystem and no screenshot tooling. It composes with
//! `tap` entirely at the caller, which decodes a bag and reads whatever field
//! it knows carries a surface id; `tap` itself is untouched.
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
use base64::Engine as _;
use parking_lot::Mutex;
use serde::Deserialize;
use serde_json::{Value, json};
use streamlib::sdk::error::Result;
use streamlib::sdk::pubsub::{Event, EventListener, PUBSUB, topics};
use streamlib::sdk::runtime::{ExchangedPublishedSurfaceFramePngImage, RuntimeOperations};

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

/// Long-edge ceiling for the image an `exchange` result carries inline, and
/// the default when a caller names no cap of its own.
///
/// A vision-model ingestion ceiling — not a GPU or protocol constant.
const EXCHANGE_IMAGE_LONG_EDGE_PIXEL_CAP: u32 = 1568;

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
            "name": "exchange",
            "description": "Exchange a published surface id for that frame's pixels, returned as a PNG image block you can see directly. Ids come from bags a `tap` returned — this tool never reads a channel itself. The image is downscaled to a declared long-edge cap; the result states the surface's true extent and the REST route that returns the exact full-resolution bytes.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "surface_id": { "type": "string", "description": "A surface id a bag published, e.g. the `{slot}#{generation}` of a pooled frame. A retired id is refused rather than answered with the slot's newer pixels — tap a newer bag and exchange that." },
                    "downscale_long_edge_pixel_cap": { "type": "integer", "minimum": 1, "maximum": EXCHANGE_IMAGE_LONG_EDGE_PIXEL_CAP, "description": "Bound the returned image's long edge to this many pixels, aspect preserved and never upscaled. Defaults to the maximum, and a larger value is clamped to it: full resolution is the REST route's job, never an inline block." }
                },
                "required": ["surface_id"],
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
        "exchange" => call_exchange(runtime, arguments).await,
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
    // `subscribe` blocks until its iceoryx2 subscriber is registered, so it
    // must not run on an async worker.
    let listener_for_subscription: Arc<Mutex<dyn EventListener>> = listener.clone();
    match tokio::task::spawn_blocking(move || {
        PUBSUB.subscribe(topics::ALL, listener_for_subscription)
    })
    .await
    {
        Ok(Ok(())) => {}
        // Without a subscriber the sample would be an honest-looking zero.
        Ok(Err(subscribe_error)) => {
            return tool_error(format!("logs subscription: {subscribe_error}"));
        }
        Err(join_error) => {
            return tool_error(format!("event subscribe task failed to join: {join_error}"));
        }
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

/// Exchange a published surface id for that frame's pixels, inline.
///
/// The cap defaults to [`EXCHANGE_IMAGE_LONG_EDGE_PIXEL_CAP`] and is clamped
/// to it: a caller may ask for less than the ceiling and never more.
async fn call_exchange(runtime: &Arc<dyn RuntimeOperations>, arguments: Value) -> Value {
    // The catalog advertises `additionalProperties: false`, and here that is
    // enforced rather than advisory: a misspelled cap key would otherwise be
    // dropped and answered with a differently-sized picture.
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct ExchangeArgs {
        surface_id: String,
        #[serde(default)]
        downscale_long_edge_pixel_cap: Option<u32>,
    }
    let ExchangeArgs {
        surface_id,
        downscale_long_edge_pixel_cap,
    } = match serde_json::from_value(arguments) {
        Ok(args) => args,
        Err(e) => return tool_error(format!("exchange arguments: {e}")),
    };
    let long_edge_pixel_cap = downscale_long_edge_pixel_cap
        .unwrap_or(EXCHANGE_IMAGE_LONG_EDGE_PIXEL_CAP)
        .clamp(1, EXCHANGE_IMAGE_LONG_EDGE_PIXEL_CAP);

    match runtime
        .exchange_published_surface_id_for_png_image_bytes_async(
            surface_id.clone(),
            Some(long_edge_pixel_cap),
        )
        .await
    {
        Ok(exchanged) => {
            exchanged_frame_image_tool_call_result(&surface_id, long_edge_pixel_cap, &exchanged)
        }
        Err(e) => tool_error(format!("exchange failed: {e}")),
    }
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
    tool_ok_content_blocks(vec![json_text_content_block(&value)])
}

/// The successful `tools/call` envelope around whatever blocks a tool built.
fn tool_ok_content_blocks(content_blocks: Vec<Value>) -> Value {
    json!({ "content": content_blocks, "isError": false })
}

/// One pretty-JSON text content block — how every tool here states a result a
/// caller parses.
fn json_text_content_block(value: &Value) -> Value {
    let text = serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string());
    json!({ "type": "text", "text": text })
}

/// One PNG image content block, base64 as the MCP content encoding requires.
fn png_image_content_block(png_image_bytes: &[u8]) -> Value {
    json!({
        "type": "image",
        "data": base64::engine::general_purpose::STANDARD.encode(png_image_bytes),
        "mimeType": "image/png",
    })
}

/// A successful `exchange` result: the frame as an image block the host
/// renders in-session, then a text block stating what the surface itself
/// carries and where the exact bytes live.
///
/// Both extents are reported because they differ whenever the cap applied, and
/// a downscaled picture whose true resolution went unsaid is a measurement
/// waiting to be wrong.
fn exchanged_frame_image_tool_call_result(
    published_surface_id: &str,
    downscale_long_edge_pixel_cap: u32,
    exchanged: &ExchangedPublishedSurfaceFramePngImage,
) -> Value {
    let stated = json!({
        "surface_id": published_surface_id,
        "source_surface_pixel_width": exchanged.source_surface_pixel_width,
        "source_surface_pixel_height": exchanged.source_surface_pixel_height,
        "encoded_image_pixel_width": exchanged.encoded_image_pixel_width,
        "encoded_image_pixel_height": exchanged.encoded_image_pixel_height,
        "downscale_long_edge_pixel_cap": downscale_long_edge_pixel_cap,
        "exact_bytes_rest_route": format!(
            "GET {}",
            crate::handlers::surface_image_exchange_route_path_for_surface_id(published_surface_id)
        ),
    });
    tool_ok_content_blocks(vec![
        png_image_content_block(&exchanged.png_image_bytes),
        json_text_content_block(&stated),
    ])
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

    use crate::control_plane_stub_support::{
        STUB_EXCHANGED_FRAME_SURFACE_ID, STUB_EXCHANGED_FRAME_SURFACE_ID_PERCENT_ENCODED,
        STUB_EXCHANGED_IMAGE_BYTES, STUB_SOURCE_SURFACE_EXTENT, StubSurfaceExchange,
    };
    use axum::Router;
    use axum::body::Body;
    use axum::http::{Request, StatusCode, header::CONTENT_TYPE};
    use base64::Engine as _;
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
        exchange: StubSurfaceExchange,
    }

    impl ControlPlaneMcpDispatchStubRuntime {
        fn new() -> Self {
            Self {
                tap_plan: None,
                recorded_shutdown_reasons: Arc::new(Mutex::new(Vec::new())),
                exchange: StubSurfaceExchange::default(),
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
        crate::control_plane_stub_support::surface_exchange_op_answers_the_stub!();
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
    const OBSERVATION_TOOL_NAMES: &[&str] = &["graph", "tap", "logs", "exchange", "shutdown"];

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
    async fn tools_call_logs_returns_bounded_window_sample() {
        // Hermetic: PUBSUB is uninitialized here, so no event is delivered and
        // the collection is bounded by the monotonic sample window, returning an
        // empty sample rather than hanging. Live event delivery rides iceoryx2
        // and is exercised by the engine's pubsub integration tests, not here.
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
        assert!(
            elapsed < LOGS_SAMPLE_WINDOW * 4,
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

    // ------------------------------------------------------------------
    // exchange: a published surface id in, an image content block out
    // ------------------------------------------------------------------

    fn exchange_stub(exchange: StubSurfaceExchange) -> Arc<ControlPlaneMcpDispatchStubRuntime> {
        Arc::new(ControlPlaneMcpDispatchStubRuntime {
            exchange,
            ..ControlPlaneMcpDispatchStubRuntime::new()
        })
    }

    /// Drive `tools/call` on `exchange` against a default stub, returning the
    /// `result` object and the `(surface id, cap)` pairs the tool handed the
    /// operation.
    async fn call_exchange_tool(arguments: Value) -> (Value, Vec<(String, Option<u32>)>) {
        let (body, calls) =
            call_exchange_tool_on(exchange_stub(StubSurfaceExchange::default()), arguments).await;
        (body["result"].clone(), calls)
    }

    /// The same call against a stub the test chose, returning the whole
    /// JSON-RPC body — so a refusal test can assert it is an in-band tool
    /// error and not a JSON-RPC one.
    async fn call_exchange_tool_on(
        runtime: Arc<ControlPlaneMcpDispatchStubRuntime>,
        arguments: Value,
    ) -> (Value, Vec<(String, Option<u32>)>) {
        let recorded = runtime.exchange.recorded_calls.clone();
        let (status, body) = mcp_call(
            runtime,
            json!({
                "jsonrpc": "2.0", "id": 20, "method": "tools/call",
                "params": { "name": "exchange", "arguments": arguments }
            }),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let calls = recorded.lock().clone();
        (body, calls)
    }

    fn content_block_of_type<'a>(result: &'a Value, block_type: &str) -> &'a Value {
        result["content"]
            .as_array()
            .unwrap_or_else(|| panic!("a content array; got {result}"))
            .iter()
            .find(|block| block["type"] == block_type)
            .unwrap_or_else(|| panic!("a `{block_type}` content block; got {result}"))
    }

    /// The whole point of the MCP spelling: the frame arrives *in the
    /// session* as a picture, not as a path or a URL the host would have to
    /// fetch — which is also why a host on another machine needs no shared
    /// filesystem.
    #[tokio::test]
    async fn tools_call_exchange_returns_the_frame_as_a_renderable_png_image_block() {
        let (result, _) =
            call_exchange_tool(json!({ "surface_id": STUB_EXCHANGED_FRAME_SURFACE_ID })).await;

        assert_eq!(result["isError"], false, "result={result}");
        let image = content_block_of_type(&result, "image");
        assert_eq!(image["mimeType"], "image/png");
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(image["data"].as_str().expect("base64 image data"))
            .expect("the image block carries decodable base64");
        assert_eq!(
            decoded, STUB_EXCHANGED_IMAGE_BYTES,
            "the tool re-encodes nothing: the block is base64 of the operation's own bytes"
        );
    }

    /// A downscaled picture without its true extent is a measurement trap —
    /// so the text block states what the surface actually carries, which id
    /// it answered, and the one route that returns the exact bytes.
    #[tokio::test]
    async fn tools_call_exchange_states_the_true_extent_the_id_and_the_exact_bytes_route() {
        let (result, _) =
            call_exchange_tool(json!({ "surface_id": STUB_EXCHANGED_FRAME_SURFACE_ID })).await;

        let text = content_block_of_type(&result, "text")["text"]
            .as_str()
            .expect("text content block");
        let stated: Value = serde_json::from_str(text).expect("the text block is JSON");

        let (source_pixel_width, source_pixel_height) = STUB_SOURCE_SURFACE_EXTENT;
        assert_eq!(stated["surface_id"], STUB_EXCHANGED_FRAME_SURFACE_ID);
        assert_eq!(stated["source_surface_pixel_width"], source_pixel_width);
        assert_eq!(stated["source_surface_pixel_height"], source_pixel_height);
        assert_eq!(
            stated["encoded_image_pixel_width"], EXCHANGE_IMAGE_LONG_EDGE_PIXEL_CAP,
            "the inline image is the downscaled one, and says so"
        );
        assert_eq!(
            stated["encoded_image_pixel_height"],
            source_pixel_height * EXCHANGE_IMAGE_LONG_EDGE_PIXEL_CAP / source_pixel_width,
            "the short edge takes the cap's ratio, and stating it is what shows the two \
             extents differ at all"
        );
        assert_eq!(
            stated["exact_bytes_rest_route"],
            format!("GET /api/surfaces/{STUB_EXCHANGED_FRAME_SURFACE_ID_PERCENT_ENCODED}/image"),
            "the `#` of a frame id must be percent-encoded or the route names a fragment"
        );
    }

    /// The route the tool points at has to be the route the server serves;
    /// a drifting path would send an agent chasing a 404 for exact bytes.
    #[tokio::test]
    async fn the_exact_bytes_route_the_tool_names_is_the_route_the_spec_serves() {
        let (result, _) =
            call_exchange_tool(json!({ "surface_id": STUB_EXCHANGED_FRAME_SURFACE_ID })).await;
        let text = content_block_of_type(&result, "text")["text"]
            .as_str()
            .expect("text content block");
        let stated: Value = serde_json::from_str(text).unwrap();
        let named_route = stated["exact_bytes_rest_route"].as_str().unwrap();

        let served = crate::handlers::SURFACE_IMAGE_EXCHANGE_ROUTE_PATH_TEMPLATE.replace(
            "{surface_id}",
            STUB_EXCHANGED_FRAME_SURFACE_ID_PERCENT_ENCODED,
        );
        assert_eq!(named_route, format!("GET {served}"));
        assert!(
            crate::handlers::control_plane_openapi_spec()
                .paths
                .paths
                .contains_key(crate::handlers::SURFACE_IMAGE_EXCHANGE_ROUTE_PATH_TEMPLATE),
            "the route the tool names must exist in the served spec"
        );
    }

    /// Downscaled *by default*: a caller that names no cap still gets an
    /// image bounded to the declared ceiling, because a full-resolution
    /// frame inline is the one banned combination.
    #[tokio::test]
    async fn tools_call_exchange_applies_the_declared_cap_when_the_caller_names_none() {
        let (result, calls) =
            call_exchange_tool(json!({ "surface_id": STUB_EXCHANGED_FRAME_SURFACE_ID })).await;

        assert_eq!(result["isError"], false, "result={result}");
        assert_eq!(
            calls,
            vec![(
                STUB_EXCHANGED_FRAME_SURFACE_ID.to_string(),
                Some(EXCHANGE_IMAGE_LONG_EDGE_PIXEL_CAP)
            )],
            "the operation must never be handed `None` from this front end"
        );
    }

    #[tokio::test]
    async fn tools_call_exchange_honours_a_caller_cap_below_the_declared_ceiling() {
        let (_, calls) = call_exchange_tool(json!({
            "surface_id": STUB_EXCHANGED_FRAME_SURFACE_ID,
            "downscale_long_edge_pixel_cap": 512
        }))
        .await;

        assert_eq!(
            calls,
            vec![(STUB_EXCHANGED_FRAME_SURFACE_ID.to_string(), Some(512))]
        );
    }

    /// Full resolution lives on REST and nowhere else, so a cap above the
    /// declared ceiling is clamped rather than obeyed — an agent cannot ask
    /// its way into a payload its own API will refuse.
    #[tokio::test]
    async fn tools_call_exchange_clamps_a_caller_cap_above_the_declared_ceiling() {
        let (result, calls) = call_exchange_tool(json!({
            "surface_id": STUB_EXCHANGED_FRAME_SURFACE_ID,
            "downscale_long_edge_pixel_cap": 4096
        }))
        .await;

        assert_eq!(result["isError"], false, "result={result}");
        assert_eq!(
            calls,
            vec![(
                STUB_EXCHANGED_FRAME_SURFACE_ID.to_string(),
                Some(EXCHANGE_IMAGE_LONG_EDGE_PIXEL_CAP)
            )],
            "a larger cap is clamped to the ceiling, never passed through"
        );
    }

    /// A retired id is an in-band tool error naming the recycling, so the
    /// agent reads why its call returned no picture and taps a newer bag —
    /// never a JSON-RPC error, and never the slot's newer pixels.
    #[tokio::test]
    async fn tools_call_exchange_on_a_recycled_frame_is_a_tool_error_naming_the_recycling() {
        let (body, _) = call_exchange_tool_on(
            exchange_stub(StubSurfaceExchange::refusing_as_recycled(
                STUB_EXCHANGED_FRAME_SURFACE_ID,
            )),
            json!({ "surface_id": STUB_EXCHANGED_FRAME_SURFACE_ID }),
        )
        .await;

        assert!(body.get("error").is_none(), "not a JSON-RPC error: {body}");
        let result = &body["result"];
        assert_eq!(result["isError"], true, "body={body}");
        let reported = result["content"][0]["text"].as_str().unwrap();
        assert!(
            reported.contains(STUB_EXCHANGED_FRAME_SURFACE_ID),
            "the refusal must name the id asked for: {reported}"
        );
        assert!(
            reported.contains("recycled"),
            "the refusal must say the frame was recycled: {reported}"
        );
        assert!(
            result["content"]
                .as_array()
                .is_some_and(|content| content.iter().all(|block| block["type"] == "text")),
            "a refusal carries no image block: {body}"
        );
    }

    /// Arguments the schema forbids never reach the runtime, and say why in
    /// band. The misspelled-key case is the one that would otherwise be
    /// silent: dropped, then answered with a differently-sized picture.
    #[tokio::test]
    async fn tools_call_exchange_with_arguments_the_schema_forbids_is_an_in_band_tool_error() {
        for (case, arguments) in [
            (
                "no surface id",
                json!({ "downscale_long_edge_pixel_cap": 64 }),
            ),
            (
                "a cap of the wrong type",
                json!({ "surface_id": STUB_EXCHANGED_FRAME_SURFACE_ID, "downscale_long_edge_pixel_cap": "big" }),
            ),
            (
                "a misspelled cap key",
                json!({ "surface_id": STUB_EXCHANGED_FRAME_SURFACE_ID, "downscal_long_edge_pixel_cap": 512 }),
            ),
        ] {
            let (body, calls) =
                call_exchange_tool_on(exchange_stub(StubSurfaceExchange::default()), arguments)
                    .await;

            assert!(
                body.get("error").is_none(),
                "{case} must not be a JSON-RPC error: {body}"
            );
            assert_eq!(body["result"]["isError"], true, "{case}: {body}");
            assert!(
                body["result"]["content"][0]["text"]
                    .as_str()
                    .unwrap_or_default()
                    .contains("exchange arguments"),
                "{case} must name the offending argument set: {body}"
            );
            assert!(
                calls.is_empty(),
                "{case} must not reach the runtime: {calls:?}"
            );
        }
    }

    /// Tap's arguments are its whole contract with a caller: no new
    /// argument, nothing renamed, nothing removed by a tool joining the
    /// catalog beside it.
    #[tokio::test]
    async fn the_tap_tool_schema_is_unchanged_by_the_exchange_joining_the_catalog() {
        let (_, body) = mcp_call(
            Arc::new(ControlPlaneMcpDispatchStubRuntime::new()),
            json!({ "jsonrpc": "2.0", "id": 24, "method": "tools/list" }),
        )
        .await;

        let tap = body["result"]["tools"]
            .as_array()
            .expect("tools array")
            .iter()
            .find(|tool| tool["name"] == "tap")
            .expect("the tap tool");
        let mut argument_names: Vec<&str> = tap["inputSchema"]["properties"]
            .as_object()
            .expect("tap declares properties")
            .keys()
            .map(String::as_str)
            .collect();
        argument_names.sort_unstable();
        assert_eq!(argument_names, ["channel", "count"]);
        assert_eq!(tap["inputSchema"]["required"], json!(["channel"]));
    }
}
