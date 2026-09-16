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
//! live graph the same way the REST client does, and beside them serves the
//! processor catalog and the live graph as *resources*
//! ([`crate::mcp_resources`]) and recipes over those tools as *prompts*
//! ([`crate::mcp_prompts`]).
//!
//! The vocabulary is the observation verbs — graph, tap, logs, exchange,
//! shutdown — beside the four graph-mutation verbs the engine's own runtime
//! API has always had: `add_processor`, `remove_processor`, `connect` and
//! `disconnect`. A mutation tool answers when the engine accepted the change
//! into its graph; the wiring itself commits on the engine's own compile task,
//! whose failure `graph` and `logs` show rather than this call.
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
use streamlib::sdk::descriptors::ProcessorClassImportPath;
use streamlib::sdk::error::Result;
use streamlib::sdk::graph::{InputLinkPortRef, LinkUniqueId, OutputLinkPortRef, ProcessorUniqueId};
use streamlib::sdk::processors::ProcessorSpec;
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

/// Per-bag ceiling on the bytes a `tap` result hex-encodes, when the caller
/// names none.
///
/// Generous rather than frugal, because a trimmed bag is not a smaller answer —
/// it is no answer: a bag is a msgpack map, so a decoder needs all of it or
/// none, and every consumer here refuses a truncated one rather than reading
/// half a value.
const DEFAULT_MAX_TAP_BAG_BYTES: usize = 1024 * 1024;

/// Total bag bytes one `tap` result encodes, and therefore also the largest
/// per-bag cap a caller may name — [`bounded_tap_bag_bytes`] clamps to it.
///
/// The two are one number rather than two so the bound holds by construction:
/// were the per-bag cap allowed above this, a single bag could exceed the whole
/// response budget and nothing would stop it. Hex doubles this on the wire, and
/// serializing the result holds the encoded text and the JSON body at once,
/// which is why the figure is modest next to what a channel may carry — a bag
/// too big for a request/response tool is what `/ws/tap/{channel}` streams
/// verbatim.
const MAX_TAP_RESPONSE_BAG_BYTES: usize = 16 * 1024 * 1024;

// The default has to fit the budget it is charged against, or the very first
// bag of an ordinary call would trip it. Checked by the compiler rather than a
// test, because it is a fact about two literals.
const _: () = assert!(DEFAULT_MAX_TAP_BAG_BYTES <= MAX_TAP_RESPONSE_BAG_BYTES);

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
pub(crate) struct RpcError {
    code: i64,
    message: String,
}

/// A JSON-RPC method's answer: its result, or the error the envelope carries.
pub(crate) type RpcResult<T> = std::result::Result<T, RpcError>;

impl RpcError {
    fn method_not_found(method: &str) -> Self {
        Self {
            code: -32601,
            message: format!("method not found: {method}"),
        }
    }
    pub(crate) fn invalid_params(message: impl Into<String>) -> Self {
        Self {
            code: -32602,
            message: message.into(),
        }
    }
    pub(crate) fn internal(message: impl Into<String>) -> Self {
        Self {
            code: -32603,
            message: message.into(),
        }
    }
    /// `-32002`, the code the MCP specification assigns an unknown resource URI.
    pub(crate) fn resource_not_found(uri: &str) -> Self {
        Self {
            code: -32002,
            message: format!(
                "no resource at `{uri}`; `resources/list` names the ones this node serves"
            ),
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
) -> RpcResult<Value> {
    match method {
        "initialize" => Ok(initialize_result()),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({ "tools": tool_definitions() })),
        "tools/call" => tools_call(runtime, params).await,
        "resources/list" => Ok(crate::mcp_resources::resources_list_result()),
        "resources/templates/list" => Ok(crate::mcp_resources::resource_templates_list_result()),
        "resources/read" => crate::mcp_resources::read_resource(runtime, params).await,
        "prompts/list" => Ok(crate::mcp_prompts::prompts_list_result()),
        "prompts/get" => crate::mcp_prompts::get_prompt(runtime, params).await,
        other => Err(RpcError::method_not_found(other)),
    }
}

fn initialize_result() -> Value {
    json!({
        "protocolVersion": MCP_PROTOCOL_VERSION,
        "capabilities": {
            "tools": { "listChanged": false },
            "resources": { "subscribe": false, "listChanged": false },
            "prompts": { "listChanged": false },
        },
        "serverInfo": { "name": MCP_SERVER_NAME, "version": MCP_SERVER_VERSION },
        "instructions": "StreamLib runtime control plane for one running node. Observe it with `graph` (processors, their ids, port names and links), `tap` (raw bags on a channel spelled `<processor id, lowercased>/<output port>`), `logs` and `exchange` (a published frame's pixels). Change its live graph with `add_processor`, `connect`, `disconnect` and `remove_processor`: a Python processor class written to a module the app can import — a file beside `app.py`, or a pip-installed package — is added by its `module:ClassName` path and runs in its own helper process; a link is spliced in by connecting the new processor on both sides, then disconnecting the link it replaces. Read `graph` first for ids and port names, and again afterwards to confirm a link's state is `wired` and the processor is `Running`. A `connect` onto a processor in a helper process returns before that helper has opened its port, so its link reads `pending` until the helper answers and then `wired`; a link that reads `error` carries the helper's own reason in `error_reason` and will never carry a bag — read the reason, `disconnect` it, and fix what it names. The resource `streamlib://processor-catalog` lists every type `add_processor` can take with its config schema and ports, and `streamlib://graph` is the live graph. The prompts are step-by-step recipes over these tools: inserting a processor into a link, fanning an output to another consumer, showing a channel on a virtual camera, and looking at what a channel carries.",
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
            "description": "Export the current runtime graph (processors, links, states, metrics) and the capability extensions loaded in this process, as JSON.",
            "inputSchema": { "type": "object", "properties": {}, "additionalProperties": false },
        }),
        json!({
            "name": "tap",
            "description": "Attach a read-only tap to a channel and collect a bounded sample of raw bags (FrameHeader-framed bytes; the hex plus byte length per bag). Bags arrive whole unless one exceeds `max_bag_bytes`, which is flagged as `hex_truncated`. The whole result is also byte-budgeted: the sample stops at the first bag that would exceed it, so `bags_withheld_at_byte_budget` is 0 or 1 — that one bag was received and discarded, and it accounts for the whole gap between `requested` and `received` when the window had time left.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "channel": { "type": "string", "description": "Channel data-service name, e.g. {source_processor}/{source_output_port}." },
                    "count": { "type": "integer", "minimum": 1, "description": "Number of bags to collect before returning. Defaults to a small sample." },
                    "max_bag_bytes": { "type": "integer", "minimum": 1, "maximum": MAX_TAP_RESPONSE_BAG_BYTES, "description": "Per-bag ceiling on the bytes hex-encoded into the result. A bag over the cap comes back flagged `hex_truncated` and cannot be decoded, so raise this rather than accept one. Defaults high enough to carry any audio block whole." }
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
        json!({
            "name": "add_processor",
            "description": "Add a processor to the running graph by its class import path — the `type` string `graph` reports for every node. A Python class is named `module:QualifiedClassName` and must be importable from the app's own environment (a module beside `app.py`, or a pip-installed package); a built-in is named by the `type` an existing node of that kind shows. Returns the new processor's id, which `connect` and `remove_processor` take, once the engine has spawned the processor — a Python class in its own helper process, which imports the module fresh, so edited code is picked up by every new add. The class's port declaration is read the first time it is added and kept; to change a class's ports, add it under a new class name. Read `graph` to see its state and ports.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "type": { "type": "string", "description": "The processor class import path, e.g. `processors.grayscale_effect:GrayscaleEffect`." },
                    "config": { "type": "object", "description": "The processor's configuration, as the keys its config schema declares. Omit for none." },
                    "display_name": { "type": "string", "description": "Human-facing label; defaults to the class's short name, disambiguated within the graph." }
                },
                "required": ["type"],
                "additionalProperties": false
            },
        }),
        json!({
            "name": "remove_processor",
            "description": "Remove a processor from the running graph by id, stopping it and dropping its node. Its links go with it.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "processor_id": { "type": "string", "description": "A processor id `graph` or `add_processor` reported." }
                },
                "required": ["processor_id"],
                "additionalProperties": false
            },
        }),
        json!({
            "name": "connect",
            "description": "Link one processor's output port to another's input port. Port names are what `graph` lists under each node's `outputs` and `inputs`. Returns the link id `disconnect` takes.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "from_processor_id": { "type": "string", "description": "The source processor's id." },
                    "from_port": { "type": "string", "description": "The source's output port name." },
                    "to_processor_id": { "type": "string", "description": "The destination processor's id." },
                    "to_port": { "type": "string", "description": "The destination's input port name." }
                },
                "required": ["from_processor_id", "from_port", "to_processor_id", "to_port"],
                "additionalProperties": false
            },
        }),
        json!({
            "name": "disconnect",
            "description": "Remove a link from the running graph by id.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "link_id": { "type": "string", "description": "A link id `graph` or `connect` reported." }
                },
                "required": ["link_id"],
                "additionalProperties": false
            },
        }),
    ]
}

// ============================================================================
// tools/call dispatch
// ============================================================================

async fn tools_call(runtime: &Arc<dyn RuntimeOperations>, params: Value) -> RpcResult<Value> {
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
        "add_processor" => call_add_processor(runtime, arguments).await,
        "remove_processor" => call_remove_processor(runtime, arguments).await,
        "connect" => call_connect(runtime, arguments).await,
        "disconnect" => call_disconnect(runtime, arguments).await,
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
        #[serde(default)]
        max_bag_bytes: Option<usize>,
    }
    let TapArgs {
        channel,
        count,
        max_bag_bytes,
    } = match serde_json::from_value(arguments) {
        Ok(args) => args,
        Err(e) => return tool_error(format!("tap arguments: {e}")),
    };
    let sample = bounded_sample_count(count, DEFAULT_TAP_SAMPLE_COUNT);
    let max_bag_bytes = bounded_tap_bag_bytes(max_bag_bytes);

    let mut subscription = match runtime.tap_async(channel.clone(), Some(sample)).await {
        Ok(subscription) => subscription,
        Err(e) => return tool_error(format!("tap attach failed: {e}")),
    };

    let mut bags: Vec<Value> = Vec::with_capacity(sample);
    let mut remaining_response_bytes = MAX_TAP_RESPONSE_BAG_BYTES;
    let mut bags_withheld_at_byte_budget = 0usize;
    let deadline = tokio::time::Instant::now() + TAP_SAMPLE_WINDOW;
    while bags.len() < sample {
        match tokio::time::timeout_at(deadline, subscription.recv()).await {
            Ok(Some(bytes)) => {
                let encoded_len = bytes.len().min(max_bag_bytes);
                if encoded_len > remaining_response_bytes {
                    // Counted rather than silently eaten: this bag was received
                    // and is being dropped, so a caller reconciling `requested`
                    // against `received` is not short by an unexplained one.
                    bags_withheld_at_byte_budget += 1;
                    break;
                }
                remaining_response_bytes -= encoded_len;
                bags.push(tap_bag_json(&bytes[..encoded_len], bytes.len()));
            }
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
        "max_bag_bytes": max_bag_bytes,
        "bags_withheld_at_byte_budget": bags_withheld_at_byte_budget,
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

async fn call_add_processor(runtime: &Arc<dyn RuntimeOperations>, arguments: Value) -> Value {
    #[derive(Deserialize)]
    struct AddProcessorArguments {
        #[serde(rename = "type")]
        processor_class_import_path: String,
        #[serde(default)]
        config: Option<Value>,
        #[serde(default)]
        display_name: Option<String>,
    }
    let arguments: AddProcessorArguments = match serde_json::from_value(arguments) {
        Ok(arguments) => arguments,
        Err(e) => return tool_error(format!("add_processor arguments: {e}")),
    };
    let processor_class_import_path =
        match ProcessorClassImportPath::new(&arguments.processor_class_import_path) {
            Ok(path) => path,
            Err(e) => return tool_error(format!("add_processor `type`: {e}")),
        };
    // Absent is an empty object, never null: a config struct deserializes from
    // `{}` and not from `null`.
    let config = match arguments.config {
        None | Some(Value::Null) => Value::Object(serde_json::Map::new()),
        Some(object @ Value::Object(_)) => object,
        Some(other) => {
            return tool_error(format!(
                "add_processor `config` must be a JSON object, got {other}"
            ));
        }
    };
    let mut spec = ProcessorSpec::new(processor_class_import_path, config);
    spec.display_name = arguments.display_name;

    match runtime.add_processor_async(spec).await {
        Ok(processor_id) => tool_ok(json!({ "processor_id": processor_id.as_str() })),
        Err(e) => tool_error(format!("add_processor failed: {e}")),
    }
}

async fn call_remove_processor(runtime: &Arc<dyn RuntimeOperations>, arguments: Value) -> Value {
    #[derive(Deserialize)]
    struct RemoveProcessorArguments {
        processor_id: String,
    }
    let arguments: RemoveProcessorArguments = match serde_json::from_value(arguments) {
        Ok(arguments) => arguments,
        Err(e) => return tool_error(format!("remove_processor arguments: {e}")),
    };
    let processor_id = ProcessorUniqueId::from(arguments.processor_id.as_str());
    match runtime.remove_processor_async(processor_id).await {
        Ok(()) => tool_ok(json!({ "removed_processor_id": arguments.processor_id })),
        Err(e) => tool_error(format!("remove_processor failed: {e}")),
    }
}

async fn call_connect(runtime: &Arc<dyn RuntimeOperations>, arguments: Value) -> Value {
    #[derive(Deserialize)]
    struct ConnectArguments {
        from_processor_id: String,
        from_port: String,
        to_processor_id: String,
        to_port: String,
    }
    let arguments: ConnectArguments = match serde_json::from_value(arguments) {
        Ok(arguments) => arguments,
        Err(e) => return tool_error(format!("connect arguments: {e}")),
    };
    let from = OutputLinkPortRef::new(
        ProcessorUniqueId::from(arguments.from_processor_id.as_str()),
        arguments.from_port,
    );
    let to = InputLinkPortRef::new(
        ProcessorUniqueId::from(arguments.to_processor_id.as_str()),
        arguments.to_port,
    );
    match runtime.connect_async(from, to).await {
        Ok(link_id) => tool_ok(json!({ "link_id": link_id.as_str() })),
        Err(e) => tool_error(format!("connect failed: {e}")),
    }
}

async fn call_disconnect(runtime: &Arc<dyn RuntimeOperations>, arguments: Value) -> Value {
    #[derive(Deserialize)]
    struct DisconnectArguments {
        link_id: String,
    }
    let arguments: DisconnectArguments = match serde_json::from_value(arguments) {
        Ok(arguments) => arguments,
        Err(e) => return tool_error(format!("disconnect arguments: {e}")),
    };
    let link_id = LinkUniqueId::from(arguments.link_id.as_str());
    match runtime.disconnect_async(link_id).await {
        Ok(()) => tool_ok(json!({ "disconnected_link_id": arguments.link_id })),
        Err(e) => tool_error(format!("disconnect failed: {e}")),
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

/// Clamp a requested per-bag cap into `[1, MAX_TAP_RESPONSE_BAG_BYTES]`,
/// defaulting when the caller left it unset.
///
/// Clamping to the whole-response budget is what makes the first bag unable to
/// exceed it, so the collection loop needs no special case for one.
fn bounded_tap_bag_bytes(requested: Option<usize>) -> usize {
    requested
        .unwrap_or(DEFAULT_MAX_TAP_BAG_BYTES)
        .clamp(1, MAX_TAP_RESPONSE_BAG_BYTES)
}

/// Render one raw tap bag as JSON: the bag's full byte length plus the hex of
/// however much of it the caller's cap admitted (raw bags are wire-neutral
/// bytes; decoding is the caller's concern).
///
/// Takes the already-clamped slice rather than clamping again, because the
/// collection loop must charge its budget the same number this encodes — two
/// sites computing one rule is two sites that can disagree.
fn tap_bag_json(encoded: &[u8], full_byte_len: usize) -> Value {
    json!({
        "byte_len": full_byte_len,
        "hex_preview": hex_encode(encoded),
        "hex_truncated": encoded.len() < full_byte_len,
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
    // A nibble table rather than `write!` per byte: this encodes up to
    // `MAX_TAP_RESPONSE_BAG_BYTES` on a tokio worker, where `core::fmt`'s
    // per-byte machinery costs several times what a two-push loop does.
    const HEX_DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut hex = Vec::with_capacity(bytes.len() * 2);
    for byte in bytes {
        hex.push(HEX_DIGITS[usize::from(byte >> 4)]);
        hex.push(HEX_DIGITS[usize::from(byte & 0x0f)]);
    }
    String::from_utf8(hex).expect("a hex-digit table only ever yields ASCII")
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
    /// Every graph-mutating op records what it was handed and answers a fixed
    /// id, so a mutation tool's test asserts the op it reached and the
    /// arguments it carried.
    struct ControlPlaneMcpDispatchStubRuntime {
        exported_graph: Arc<Mutex<Value>>,
        tap_plan: Option<StubTapPlan>,
        recorded_shutdown_reasons: Arc<Mutex<Vec<String>>>,
        recorded_graph_mutations: crate::control_plane_stub_support::RecordedGraphMutations,
        exchange: StubSurfaceExchange,
    }

    impl ControlPlaneMcpDispatchStubRuntime {
        fn new() -> Self {
            Self {
                exported_graph: Arc::new(Mutex::new(json!({ "processors": [], "links": [] }))),
                tap_plan: None,
                recorded_shutdown_reasons: Arc::new(Mutex::new(Vec::new())),
                recorded_graph_mutations: Arc::new(Mutex::new(Vec::new())),
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
            let exported_graph = self.exported_graph.lock().clone();
            Box::pin(async move { Ok(exported_graph) })
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
        crate::control_plane_stub_support::graph_mutation_ops_record_the_call!();
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
    const CONTROL_TOOL_NAMES: &[&str] = &[
        "graph",
        "tap",
        "logs",
        "exchange",
        "shutdown",
        "add_processor",
        "remove_processor",
        "connect",
        "disconnect",
    ];

    fn mcp_router(runtime: Arc<dyn RuntimeOperations>) -> Router {
        crate::handlers::build_router(runtime, None)
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
    async fn initialize_handshake_reports_the_tools_resources_and_prompts_capabilities() {
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
        for capability in ["tools", "resources", "prompts"] {
            assert!(
                body["result"]["capabilities"][capability].is_object(),
                "server must advertise the {capability} capability: {body}"
            );
        }
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
    async fn tools_list_advertises_exactly_the_control_vocabulary() {
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
            names, CONTROL_TOOL_NAMES,
            "tools/list must advertise exactly the control vocabulary"
        );
        for tool in tools {
            assert_eq!(
                tool["inputSchema"]["type"], "object",
                "tool `{}` must declare an object inputSchema",
                tool["name"]
            );
        }
    }

    #[tokio::test]
    async fn tools_call_add_processor_reaches_the_runtime_op_with_the_spec() {
        let runtime = Arc::new(ControlPlaneMcpDispatchStubRuntime::new());
        let recorded = runtime.recorded_graph_mutations.clone();

        let (status, body) = mcp_call(
            runtime,
            json!({
                "jsonrpc": "2.0", "id": 30, "method": "tools/call",
                "params": { "name": "add_processor", "arguments": {
                    "type": "processors.grayscale_effect:GrayscaleEffect",
                    "config": { "strength": 0.5 },
                    "display_name": "Gray"
                } }
            }),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["result"]["isError"], false, "body={body}");
        let text = body["result"]["content"][0]["text"].as_str().unwrap();
        let stated: Value = serde_json::from_str(text).unwrap();
        assert_eq!(
            stated["processor_id"],
            crate::control_plane_stub_support::STUB_ADDED_PROCESSOR_ID
        );
        let recorded = recorded.lock();
        let [crate::control_plane_stub_support::RecordedGraphMutation::AddProcessor(spec)] =
            &recorded[..]
        else {
            panic!("add_processor must reach exactly one add op, recorded {recorded:?}");
        };
        assert_eq!(
            spec.name.as_str(),
            "processors.grayscale_effect:GrayscaleEffect"
        );
        assert_eq!(spec.config["strength"], 0.5);
        assert_eq!(spec.display_name.as_deref(), Some("Gray"));
    }

    #[tokio::test]
    async fn tools_call_add_processor_without_config_sends_an_empty_object_not_null() {
        let runtime = Arc::new(ControlPlaneMcpDispatchStubRuntime::new());
        let recorded = runtime.recorded_graph_mutations.clone();

        let (_, body) = mcp_call(
            runtime,
            json!({
                "jsonrpc": "2.0", "id": 31, "method": "tools/call",
                "params": { "name": "add_processor", "arguments": { "type": "streamlib:CameraSource" } }
            }),
        )
        .await;

        assert_eq!(body["result"]["isError"], false, "body={body}");
        let recorded = recorded.lock();
        let [crate::control_plane_stub_support::RecordedGraphMutation::AddProcessor(spec)] =
            &recorded[..]
        else {
            panic!("expected one add op, recorded {recorded:?}");
        };
        assert_eq!(spec.config, json!({}));
        assert_eq!(spec.display_name, None);
    }

    #[tokio::test]
    async fn tools_call_connect_and_disconnect_reach_their_runtime_ops() {
        let runtime = Arc::new(ControlPlaneMcpDispatchStubRuntime::new());
        let recorded = runtime.recorded_graph_mutations.clone();

        let (_, connect_body) = mcp_call(
            runtime.clone(),
            json!({
                "jsonrpc": "2.0", "id": 32, "method": "tools/call",
                "params": { "name": "connect", "arguments": {
                    "from_processor_id": "cam-1", "from_port": "video",
                    "to_processor_id": "fx-1", "to_port": "video_from_upstream"
                } }
            }),
        )
        .await;
        assert_eq!(
            connect_body["result"]["isError"], false,
            "body={connect_body}"
        );
        let stated: Value = serde_json::from_str(
            connect_body["result"]["content"][0]["text"]
                .as_str()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            stated["link_id"],
            crate::control_plane_stub_support::STUB_CREATED_LINK_ID
        );

        let (_, disconnect_body) = mcp_call(
            runtime,
            json!({
                "jsonrpc": "2.0", "id": 33, "method": "tools/call",
                "params": { "name": "disconnect", "arguments": { "link_id": "link-9" } }
            }),
        )
        .await;
        assert_eq!(
            disconnect_body["result"]["isError"], false,
            "body={disconnect_body}"
        );

        let recorded = recorded.lock();
        let [
            crate::control_plane_stub_support::RecordedGraphMutation::Connect(from, to),
            crate::control_plane_stub_support::RecordedGraphMutation::Disconnect(link_id),
        ] = &recorded[..]
        else {
            panic!("expected a connect then a disconnect, recorded {recorded:?}");
        };
        assert_eq!(from.processor_id.as_str(), "cam-1");
        assert_eq!(from.port_name, "video");
        assert_eq!(to.processor_id.as_str(), "fx-1");
        assert_eq!(to.port_name, "video_from_upstream");
        assert_eq!(link_id.as_str(), "link-9");
    }

    #[tokio::test]
    async fn tools_call_remove_processor_reaches_the_runtime_op() {
        let runtime = Arc::new(ControlPlaneMcpDispatchStubRuntime::new());
        let recorded = runtime.recorded_graph_mutations.clone();

        let (_, body) = mcp_call(
            runtime,
            json!({
                "jsonrpc": "2.0", "id": 34, "method": "tools/call",
                "params": { "name": "remove_processor", "arguments": { "processor_id": "fx-1" } }
            }),
        )
        .await;
        assert_eq!(body["result"]["isError"], false, "body={body}");

        let recorded = recorded.lock();
        let [crate::control_plane_stub_support::RecordedGraphMutation::RemoveProcessor(id)] =
            &recorded[..]
        else {
            panic!("expected one remove op, recorded {recorded:?}");
        };
        assert_eq!(id.as_str(), "fx-1");
    }

    #[tokio::test]
    async fn a_mutation_tool_with_malformed_arguments_is_a_tool_error_that_reaches_no_op() {
        let runtime = Arc::new(ControlPlaneMcpDispatchStubRuntime::new());
        let recorded = runtime.recorded_graph_mutations.clone();

        let (status, body) = mcp_call(
            runtime,
            json!({
                "jsonrpc": "2.0", "id": 35, "method": "tools/call",
                "params": { "name": "connect", "arguments": { "from_processor_id": "cam-1" } }
            }),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["result"]["isError"], true, "body={body}");
        assert!(
            recorded.lock().is_empty(),
            "a refused call must reach no runtime op"
        );
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

    /// One 1024-sample stereo `f32` `AudioBlock`, msgpack-framed — the exact
    /// size this rig's PipeWire arm publishes.
    const AUDIO_BLOCK_BAG_BYTES: usize = 8366;

    async fn tap_sample_from(
        runtime: Arc<ControlPlaneMcpDispatchStubRuntime>,
        arguments: Value,
    ) -> Value {
        let (status, body) = mcp_call(
            runtime,
            json!({
                "jsonrpc": "2.0", "id": 10, "method": "tools/call",
                "params": { "name": "tap", "arguments": arguments }
            }),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["result"]["isError"], false, "body={body}");
        serde_json::from_str(body["result"]["content"][0]["text"].as_str().unwrap())
            .expect("tap result text is JSON")
    }

    /// A bag is a msgpack map, so a decoder needs all of it or none — which
    /// makes the default cap load-bearing for every data model that rides its
    /// payload inline, audio first among them.
    ///
    /// Mental revert: set the default cap under 8 366 and this reddens.
    #[tokio::test]
    async fn tools_call_tap_carries_a_whole_audio_block_without_being_asked_to() {
        let audio_bag = vec![0xABu8; AUDIO_BLOCK_BAG_BYTES];
        let runtime = Arc::new(ControlPlaneMcpDispatchStubRuntime::with_tap_bags(
            vec![audio_bag],
            0,
        ));

        let sample = tap_sample_from(runtime, json!({ "channel": "mic/audio" })).await;

        let bag = &sample["bags"][0];
        assert_eq!(
            bag["byte_len"].as_u64().unwrap(),
            AUDIO_BLOCK_BAG_BYTES as u64
        );
        assert_eq!(
            bag["hex_truncated"], false,
            "an audio block must arrive whole with no caller-supplied cap"
        );
        assert_eq!(
            bag["hex_preview"].as_str().unwrap().len(),
            AUDIO_BLOCK_BAG_BYTES * 2,
            "the hex is the whole bag, so a consumer can decode it"
        );
    }

    /// The bound the response budget's doc claims, held where it was false: a
    /// caller cannot name a per-bag cap above the whole-response budget, so one
    /// bag can never exceed it and the loop needs no exemption for the first.
    ///
    /// Mental revert: clamp `bounded_tap_bag_bytes` to anything larger and this
    /// reddens — which is what a 64 MiB ceiling against a 16 MiB budget did.
    #[test]
    fn a_per_bag_cap_can_never_be_named_above_the_whole_response_budget() {
        assert_eq!(
            bounded_tap_bag_bytes(Some(MAX_TAP_RESPONSE_BAG_BYTES * 4)),
            MAX_TAP_RESPONSE_BAG_BYTES
        );
        assert_eq!(bounded_tap_bag_bytes(Some(0)), 1);
        assert_eq!(bounded_tap_bag_bytes(None), DEFAULT_MAX_TAP_BAG_BYTES);
    }

    /// A single bag larger than any cap could admit still comes back — trimmed
    /// and flagged — rather than the sample being empty. This is the case the
    /// removed first-bag exemption used to serve, and it now holds because the
    /// cap cannot exceed the budget rather than because of a special case.
    #[tokio::test]
    async fn one_bag_larger_than_the_budget_is_returned_trimmed_rather_than_withheld() {
        let runtime = Arc::new(ControlPlaneMcpDispatchStubRuntime::with_tap_bags(
            vec![vec![0x5Au8; MAX_TAP_RESPONSE_BAG_BYTES + 4096]],
            0,
        ));

        let sample = tap_sample_from(
            runtime,
            json!({ "channel": "big/bag", "max_bag_bytes": MAX_TAP_RESPONSE_BAG_BYTES }),
        )
        .await;

        assert_eq!(sample["received"], 1, "the sample is not empty");
        assert_eq!(sample["bags_withheld_at_byte_budget"], 0);
        assert_eq!(sample["bags"][0]["hex_truncated"], true);
        assert_eq!(
            sample["bags"][0]["byte_len"].as_u64().unwrap(),
            (MAX_TAP_RESPONSE_BAG_BYTES + 4096) as u64,
            "the bag's true size is reported however much of it was encoded"
        );
    }

    /// A bag over the cap is still reported and still flagged — the escape
    /// hatch is naming a bigger cap, not guessing at half a value.
    #[tokio::test]
    async fn tools_call_tap_honours_a_caller_named_per_bag_cap() {
        let bag_bytes = DEFAULT_MAX_TAP_BAG_BYTES + 4096;
        let runtime = Arc::new(ControlPlaneMcpDispatchStubRuntime::with_tap_bags(
            vec![vec![0xCDu8; bag_bytes]],
            0,
        ));

        let trimmed = tap_sample_from(Arc::clone(&runtime), json!({ "channel": "big/bag" })).await;
        assert_eq!(trimmed["bags"][0]["hex_truncated"], true);
        assert_eq!(trimmed["max_bag_bytes"], DEFAULT_MAX_TAP_BAG_BYTES);

        let runtime = Arc::new(ControlPlaneMcpDispatchStubRuntime::with_tap_bags(
            vec![vec![0xCDu8; bag_bytes]],
            0,
        ));
        let whole = tap_sample_from(
            runtime,
            json!({ "channel": "big/bag", "max_bag_bytes": bag_bytes }),
        )
        .await;
        assert_eq!(
            whole["bags"][0]["hex_truncated"], false,
            "a caller who names a cap big enough gets the bag whole"
        );
    }

    /// `count` times the per-bag cap multiplies to gigabytes at the extremes,
    /// so the response carries its own budget. It stops the sample rather than
    /// trimming, because whole bags are the only ones worth returning.
    #[tokio::test]
    async fn tools_call_tap_stops_at_its_response_budget_rather_than_trimming() {
        let one_mib_bag = vec![0xEFu8; DEFAULT_MAX_TAP_BAG_BYTES];
        let bags_that_exceed_the_budget =
            MAX_TAP_RESPONSE_BAG_BYTES / DEFAULT_MAX_TAP_BAG_BYTES + 4;
        let runtime = Arc::new(ControlPlaneMcpDispatchStubRuntime::with_tap_bags(
            vec![one_mib_bag; bags_that_exceed_the_budget],
            0,
        ));

        let sample = tap_sample_from(
            runtime,
            json!({ "channel": "big/bag", "count": bags_that_exceed_the_budget }),
        )
        .await;

        assert_eq!(
            sample["bags_withheld_at_byte_budget"], 1,
            "the bag the budget refused was received, so it is counted rather \
             than leaving `received` short for no stated reason"
        );
        let bags = sample["bags"].as_array().expect("bags array");
        assert!(
            bags.len() < bags_that_exceed_the_budget,
            "the budget must stop the sample short of what was asked for"
        );
        assert!(
            bags.iter().all(|bag| bag["hex_truncated"] == false),
            "every bag the budget did admit is whole"
        );
    }

    #[tokio::test]
    async fn tools_call_tap_shapes_bags_and_reports_dropped_count() {
        let big_bag = vec![0xABu8; DEFAULT_MAX_TAP_BAG_BYTES + 512];
        // Spans both nibble halves and every digit class, so the hex table is
        // pinned for case and for the six letters — `010203` alone leaves an
        // uppercase or transposed table green.
        let small_bag = vec![0x01u8, 0x02, 0x03, 0xAB, 0xCD, 0xEF];
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
        // Big bag: the full byte length is reported, the hex stops at the
        // per-bag cap, and truncation is flagged.
        assert_eq!(
            bags[0]["byte_len"].as_u64().unwrap(),
            (DEFAULT_MAX_TAP_BAG_BYTES + 512) as u64
        );
        assert_eq!(bags[0]["hex_truncated"], true);
        assert_eq!(
            bags[0]["hex_preview"].as_str().unwrap().len(),
            DEFAULT_MAX_TAP_BAG_BYTES * 2,
            "the hex is exactly the first DEFAULT_MAX_TAP_BAG_BYTES bytes"
        );
        // Small bag: previewed whole, not truncated.
        assert_eq!(bags[1]["byte_len"].as_u64().unwrap(), 6);
        assert_eq!(bags[1]["hex_truncated"], false);
        assert_eq!(bags[1]["hex_preview"], "010203abcdef");
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
        assert_eq!(argument_names, ["channel", "count", "max_bag_bytes"]);
        assert_eq!(
            tap["inputSchema"]["required"],
            json!(["channel"]),
            "every argument beyond the channel stays optional, so the ordinary \
             call is still `tap <channel>`"
        );
    }

    // ------------------------------------------------------------------------
    // Resources and prompts
    // ------------------------------------------------------------------------

    use streamlib::sdk::descriptors::{
        PortDescriptor, ProcessorClassShortName, ProcessorDescriptor,
    };
    use streamlib::sdk::processors::PROCESSOR_REGISTRY;

    /// Two processors and the one link between them, in the shape the engine's
    /// graph export takes, so the prompts parse what a real node answers.
    fn two_linked_processors_graph() -> Value {
        json!({
            "nodes": [
                {
                    "id": "PatternSourceId",
                    "type": "graph_probes:PatternSource",
                    "display_name": "pattern",
                    "ports": {
                        "inputs": [],
                        "outputs": [{ "name": "video", "description": "", "delivery_profile": null }]
                    },
                    "components": { "state": "Running" }
                },
                {
                    "id": "WindowSinkId",
                    "type": "graph_probes:WindowSink",
                    "display_name": "window",
                    "ports": {
                        "inputs": [{ "name": "video", "description": "", "delivery_profile": "newest" }],
                        "outputs": []
                    },
                    "components": { "state": "Running" }
                }
            ],
            "links": [{
                "id": "link-pattern-to-window",
                "source": { "processor_id": "PatternSourceId", "port_name": "video" },
                "target": { "processor_id": "WindowSinkId", "port_name": "video" },
                "state": "wired",
                "components": {}
            }],
            "extensions": []
        })
    }

    /// The same graph after the helper that was to open the link refused it —
    /// what a live `connect` onto a helper-placed processor leaves behind when
    /// that helper's port could not open.
    ///
    /// Kept beside the wired fixture rather than replacing its link: the
    /// prompts pick a link out of the graph they are rendered against, and an
    /// errored one has no business in the recipes' happy path.
    fn two_linked_processors_graph_whose_link_a_helper_refused() -> Value {
        let mut graph = two_linked_processors_graph();
        graph["links"][0]["state"] = json!("error");
        graph["links"][0]["error_reason"] = json!(
            "could not wire input port \"video\": BufferSizeExceedsMaxSupportedBufferSizeOfService"
        );
        graph
    }

    /// An agent is told to read a link's state, so a refused link has to reach
    /// it with the helper's own reason rather than a bare `error`.
    #[tokio::test]
    async fn tools_call_graph_carries_the_reason_a_helper_refused_a_link_for() {
        let runtime = ControlPlaneMcpDispatchStubRuntime::new();
        *runtime.exported_graph.lock() = two_linked_processors_graph_whose_link_a_helper_refused();

        let (status, body) = mcp_call(
            Arc::new(runtime),
            json!({ "jsonrpc": "2.0", "id": 9, "method": "tools/call", "params": { "name": "graph", "arguments": {} } }),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        let text = body["result"]["content"][0]["text"].as_str().unwrap();
        let graph: Value = serde_json::from_str(text).unwrap();
        assert_eq!(graph["links"][0]["state"], "error");
        assert!(
            graph["links"][0]["error_reason"]
                .as_str()
                .is_some_and(|reason| reason.contains("BufferSizeExceeds")),
            "the helper's own reason is what tells an agent what to fix; got {graph}"
        );
    }

    /// The control vocabulary tells an agent to confirm `wired` in `graph`. A
    /// link onto a helper is not wired when `connect` returns, so the same text
    /// has to say what `pending` and `error` mean or the confirmation is a
    /// guess.
    #[tokio::test]
    async fn the_instructions_and_every_wiring_prompt_say_what_pending_and_error_mean() {
        register_a_virtual_camera_sink_probe_once();
        let mut texts = vec![(
            "instructions",
            initialize_result()["instructions"]
                .as_str()
                .expect("the handshake carries instructions")
                .to_string(),
        )];
        for (recipe, arguments) in [
            (
                "insert_processor_between_linked_processors",
                json!({ "link_id": "link-pattern-to-window", "processor_type": "effects:Blur" }),
            ),
            (
                "fan_output_to_another_consumer",
                json!({ "from_processor_id": "PatternSourceId", "from_port": "video", "processor_type": "effects:Blur" }),
            ),
            (
                "show_channel_on_virtual_camera",
                json!({ "from_processor_id": "PatternSourceId", "from_port": "video" }),
            ),
        ] {
            texts.push((
                recipe,
                prompt_text(stub_serving_two_linked_processors(), recipe, arguments).await,
            ));
        }
        for (name, text) in texts {
            assert!(
                text.contains("pending") && text.contains("error_reason"),
                "{name} tells an agent to confirm a link's state but not what a link left \
                 `pending` by a helper that has not answered, or refused with a reason, \
                 means:\n{text}"
            );
        }
    }

    fn stub_serving_two_linked_processors() -> Arc<ControlPlaneMcpDispatchStubRuntime> {
        let runtime = ControlPlaneMcpDispatchStubRuntime::new();
        *runtime.exported_graph.lock() = two_linked_processors_graph();
        Arc::new(runtime)
    }

    /// The registry is process-global and refuses a second registration of a
    /// path, so the probe standing in for the built-in registers once for the
    /// whole test binary.
    fn register_a_virtual_camera_sink_probe_once() {
        static REGISTERED: std::sync::Once = std::sync::Once::new();
        REGISTERED.call_once(|| {
            PROCESSOR_REGISTRY
                .register_descriptor_only(
                    ProcessorDescriptor::new(
                        ProcessorClassShortName::new("VirtualCameraSink").unwrap(),
                        ProcessorClassImportPath::new(
                            crate::mcp_prompts::VIRTUAL_CAMERA_SINK_PROCESSOR_CLASS_IMPORT_PATH,
                        )
                        .unwrap(),
                        "a virtual-camera probe",
                    )
                    .with_config_schema(
                        json!({ "type": "object", "properties": { "name": { "type": "string" } } }),
                    )
                    .with_input(
                        PortDescriptor::new("video", "frames to present", true)
                            .with_delivery_profile("newest"),
                    ),
                )
                .expect("the virtual camera path is registered by this helper alone");
        });
    }

    async fn rpc_result(runtime: Arc<dyn RuntimeOperations>, method: &str, params: Value) -> Value {
        let (status, body) = mcp_call(
            runtime,
            json!({ "jsonrpc": "2.0", "id": 40, "method": method, "params": params }),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert!(
            body["error"].is_null(),
            "{method} answered an error: {body}"
        );
        body["result"].clone()
    }

    async fn rpc_error(runtime: Arc<dyn RuntimeOperations>, method: &str, params: Value) -> Value {
        let (status, body) = mcp_call(
            runtime,
            json!({ "jsonrpc": "2.0", "id": 41, "method": method, "params": params }),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert!(
            body["result"].is_null(),
            "{method} answered a result: {body}"
        );
        body["error"].clone()
    }

    async fn resource_document(runtime: Arc<dyn RuntimeOperations>, uri: &str) -> Value {
        let result = rpc_result(runtime, "resources/read", json!({ "uri": uri })).await;
        let contents = result["contents"].as_array().expect("a contents array");
        assert_eq!(contents.len(), 1, "one document per resource: {result}");
        assert_eq!(contents[0]["uri"], uri);
        assert_eq!(contents[0]["mimeType"], "application/json");
        serde_json::from_str(contents[0]["text"].as_str().expect("a text document"))
            .expect("the document is JSON")
    }

    async fn prompt_text(
        runtime: Arc<dyn RuntimeOperations>,
        prompt_name: &str,
        arguments: Value,
    ) -> String {
        let result = rpc_result(
            runtime,
            "prompts/get",
            json!({ "name": prompt_name, "arguments": arguments }),
        )
        .await;
        let messages = result["messages"].as_array().expect("a messages array");
        assert_eq!(messages.len(), 1, "{result}");
        assert_eq!(messages[0]["role"], "user");
        assert_eq!(messages[0]["content"]["type"], "text");
        messages[0]["content"]["text"].as_str().unwrap().to_string()
    }

    /// The tools a recipe's numbered steps call, in order — what a client
    /// following the text step by step dispatches.
    fn tool_names_the_numbered_steps_call(prompt_text: &str) -> Vec<String> {
        prompt_text
            .lines()
            .filter_map(|line| {
                let (step_number, rest) = line.split_once(". `")?;
                if step_number.is_empty() || !step_number.chars().all(|c| c.is_ascii_digit()) {
                    return None;
                }
                rest.split_once('`')
                    .map(|(tool_name, _)| tool_name.to_string())
            })
            .collect()
    }

    fn served_tool_names() -> Vec<String> {
        tool_definitions()
            .iter()
            .map(|tool| tool["name"].as_str().unwrap().to_string())
            .collect()
    }

    #[tokio::test]
    async fn resources_list_names_the_processor_catalog_and_the_live_graph() {
        let result = rpc_result(
            Arc::new(ControlPlaneMcpDispatchStubRuntime::new()),
            "resources/list",
            json!({}),
        )
        .await;

        let uris: Vec<&str> = result["resources"]
            .as_array()
            .expect("a resources array")
            .iter()
            .map(|resource| resource["uri"].as_str().unwrap())
            .collect();
        assert_eq!(uris, ["streamlib://processor-catalog", "streamlib://graph"]);
        for resource in result["resources"].as_array().unwrap() {
            assert_eq!(resource["mimeType"], "application/json", "{resource}");
            assert!(
                resource["description"]
                    .as_str()
                    .is_some_and(|d| !d.is_empty())
            );
        }

        let templates = rpc_result(
            Arc::new(ControlPlaneMcpDispatchStubRuntime::new()),
            "resources/templates/list",
            json!({}),
        )
        .await;
        assert_eq!(templates, json!({ "resourceTemplates": [] }));
    }

    /// Mental revert: render the catalog once at startup and cache it, and the
    /// second read misses the type registered after the first.
    #[tokio::test]
    async fn the_catalog_resource_renders_the_registry_as_it_stands_at_each_read() {
        let class_import_path =
            "streamlib_api_server::catalog_resource_probe::RegisteredBetweenReads";
        let entry_for_the_probe = |catalog: &Value| {
            catalog["processors"]
                .as_array()
                .expect("a processor list")
                .iter()
                .find(|entry| entry["processor_class_import_path"] == class_import_path)
                .cloned()
        };

        let before = resource_document(
            Arc::new(ControlPlaneMcpDispatchStubRuntime::new()),
            "streamlib://processor-catalog",
        )
        .await;
        assert!(entry_for_the_probe(&before).is_none());

        let config_schema = json!({
            "type": "object",
            "properties": { "strength": { "type": "number", "default": 0.5 } }
        });
        PROCESSOR_REGISTRY
            .register_descriptor_only(
                ProcessorDescriptor::new(
                    ProcessorClassShortName::new("RegisteredBetweenReads").unwrap(),
                    ProcessorClassImportPath::new(class_import_path).unwrap(),
                    "registered after the first read",
                )
                .with_config_schema(config_schema.clone())
                .with_input(
                    PortDescriptor::new("video_from_upstream", "", true)
                        .with_delivery_profile("newest"),
                ),
            )
            .expect("the probe's path is registered by this test alone");

        let after = resource_document(
            Arc::new(ControlPlaneMcpDispatchStubRuntime::new()),
            "streamlib://processor-catalog",
        )
        .await;
        let entry = entry_for_the_probe(&after).expect("the type registered between reads");
        assert_eq!(entry["config_schema"], config_schema);
        assert_eq!(entry["inputs"][0]["name"], "video_from_upstream");
        assert_eq!(entry["inputs"][0]["delivery_profile"], "newest");
    }

    #[tokio::test]
    async fn the_graph_resource_renders_the_graph_as_it_stands_at_each_read() {
        let runtime = Arc::new(ControlPlaneMcpDispatchStubRuntime::new());
        let exported_graph = runtime.exported_graph.clone();

        let before = resource_document(runtime.clone(), "streamlib://graph").await;
        assert_eq!(before, json!({ "processors": [], "links": [] }));

        *exported_graph.lock() = two_linked_processors_graph();
        let after = resource_document(runtime, "streamlib://graph").await;
        assert_eq!(after, two_linked_processors_graph());
    }

    #[tokio::test]
    async fn reading_a_resource_the_node_does_not_serve_is_refused_naming_the_uri() {
        let error = rpc_error(
            Arc::new(ControlPlaneMcpDispatchStubRuntime::new()),
            "resources/read",
            json!({ "uri": "streamlib://contracts" }),
        )
        .await;
        assert_eq!(error["code"], -32002);
        assert!(
            error["message"]
                .as_str()
                .unwrap()
                .contains("streamlib://contracts"),
            "{error}"
        );
    }

    #[tokio::test]
    async fn prompts_list_names_the_four_recipes_and_their_arguments() {
        let result = rpc_result(
            Arc::new(ControlPlaneMcpDispatchStubRuntime::new()),
            "prompts/list",
            json!({}),
        )
        .await;
        let prompts = result["prompts"].as_array().expect("a prompts array");

        let described: Vec<(String, Vec<(String, bool)>)> = prompts
            .iter()
            .map(|prompt| {
                let arguments = prompt["arguments"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|argument| {
                        (
                            argument["name"].as_str().unwrap().to_string(),
                            argument["required"].as_bool().unwrap(),
                        )
                    })
                    .collect();
                (prompt["name"].as_str().unwrap().to_string(), arguments)
            })
            .collect();
        let owned = |name: &str, arguments: &[(&str, bool)]| {
            (
                name.to_string(),
                arguments
                    .iter()
                    .map(|(argument, required)| (argument.to_string(), *required))
                    .collect::<Vec<_>>(),
            )
        };
        assert_eq!(
            described,
            vec![
                owned(
                    "insert_processor_between_linked_processors",
                    &[("link_id", true), ("processor_type", true)]
                ),
                owned(
                    "fan_output_to_another_consumer",
                    &[
                        ("from_processor_id", true),
                        ("from_port", true),
                        ("processor_type", true)
                    ]
                ),
                owned(
                    "show_channel_on_virtual_camera",
                    &[
                        ("from_processor_id", true),
                        ("from_port", true),
                        ("camera_name", false)
                    ]
                ),
                owned(
                    "look_at_what_a_channel_carries",
                    &[("from_processor_id", true), ("from_port", true)]
                ),
            ]
        );
    }

    /// A prompt is a recipe over the tool set, never a verb of its own: every
    /// step of every recipe calls a tool `tools/list` serves.
    #[tokio::test]
    async fn every_step_of_every_prompt_calls_a_tool_the_node_serves() {
        register_a_virtual_camera_sink_probe_once();
        let served = served_tool_names();

        for (prompt_name, arguments) in [
            (
                "insert_processor_between_linked_processors",
                json!({ "link_id": "link-pattern-to-window", "processor_type": "effects:Blur" }),
            ),
            (
                "fan_output_to_another_consumer",
                json!({ "from_processor_id": "PatternSourceId", "from_port": "video", "processor_type": "effects:Blur" }),
            ),
            (
                "show_channel_on_virtual_camera",
                json!({ "from_processor_id": "PatternSourceId", "from_port": "video" }),
            ),
            (
                "look_at_what_a_channel_carries",
                json!({ "from_processor_id": "PatternSourceId", "from_port": "video" }),
            ),
        ] {
            let text =
                prompt_text(stub_serving_two_linked_processors(), prompt_name, arguments).await;
            let called = tool_names_the_numbered_steps_call(&text);
            assert!(!called.is_empty(), "{prompt_name} lists no steps:\n{text}");
            for tool_name in &called {
                assert!(
                    served.contains(tool_name),
                    "{prompt_name} calls `{tool_name}`, which the node does not serve:\n{text}"
                );
            }
        }
    }

    #[tokio::test]
    async fn the_insert_prompt_splices_the_named_link_in_an_order_that_never_leaves_it_unfed() {
        let text = prompt_text(
            stub_serving_two_linked_processors(),
            "insert_processor_between_linked_processors",
            json!({ "link_id": "link-pattern-to-window", "processor_type": "effects:Blur" }),
        )
        .await;

        assert_eq!(
            tool_names_the_numbered_steps_call(&text),
            [
                "add_processor",
                "graph",
                "connect",
                "connect",
                "disconnect",
                "graph"
            ]
        );
        for named in [
            "`effects:Blur`",
            "`PatternSourceId`",
            "`WindowSinkId`",
            "`link-pattern-to-window`",
        ] {
            assert!(
                text.contains(named),
                "the recipe must name {named}:\n{text}"
            );
        }
    }

    /// The type an agent is about to add is described from the catalog as it
    /// is now, so the config keys it passes are the ones the node will take.
    #[tokio::test]
    async fn a_registered_types_catalog_entry_is_rendered_into_the_prompt() {
        let class_import_path = "streamlib_api_server::prompt_catalog_probe::GrayscaleEffect";
        PROCESSOR_REGISTRY
            .register_descriptor_only(
                ProcessorDescriptor::new(
                    ProcessorClassShortName::new("GrayscaleEffect").unwrap(),
                    ProcessorClassImportPath::new(class_import_path).unwrap(),
                    "a prompt-rendering probe",
                )
                .with_config_schema(json!({
                    "type": "object",
                    "properties": { "grayscale_strength_probe_key": { "type": "number" } }
                })),
            )
            .expect("the probe's path is registered by this test alone");

        let registered = prompt_text(
            stub_serving_two_linked_processors(),
            "fan_output_to_another_consumer",
            json!({ "from_processor_id": "PatternSourceId", "from_port": "video", "processor_type": class_import_path }),
        )
        .await;
        assert!(
            registered.contains("grayscale_strength_probe_key"),
            "a registered type's config schema must reach the recipe:\n{registered}"
        );

        let unregistered = prompt_text(
            stub_serving_two_linked_processors(),
            "fan_output_to_another_consumer",
            json!({ "from_processor_id": "PatternSourceId", "from_port": "video", "processor_type": "never_imported:Effect" }),
        )
        .await;
        assert!(
            unregistered.contains("not in this node's catalog yet"),
            "an unregistered type must be said to be absent, not described:\n{unregistered}"
        );
    }

    #[tokio::test]
    async fn the_virtual_camera_prompt_adds_the_registered_sink_on_its_own_input_port() {
        register_a_virtual_camera_sink_probe_once();

        let text = prompt_text(
            stub_serving_two_linked_processors(),
            "show_channel_on_virtual_camera",
            json!({ "from_processor_id": "PatternSourceId", "from_port": "video", "camera_name": "Desk \"cam\"" }),
        )
        .await;

        assert_eq!(
            tool_names_the_numbered_steps_call(&text),
            ["add_processor", "connect", "graph"]
        );
        assert!(
            text.contains(crate::mcp_prompts::VIRTUAL_CAMERA_SINK_PROCESSOR_CLASS_IMPORT_PATH),
            "{text}"
        );
        assert!(text.contains("`to_port`: `video`"), "{text}");
        assert!(
            text.contains(r#"{"name":"Desk \"cam\""}"#),
            "the camera name must reach the config as escaped JSON:\n{text}"
        );
    }

    #[tokio::test]
    async fn the_look_prompt_taps_the_channel_the_output_publishes_on() {
        let text = prompt_text(
            stub_serving_two_linked_processors(),
            "look_at_what_a_channel_carries",
            json!({ "from_processor_id": "PatternSourceId", "from_port": "video" }),
        )
        .await;

        assert_eq!(
            tool_names_the_numbered_steps_call(&text),
            ["tap", "exchange"]
        );
        assert!(
            text.contains("`channel`: `patternsourceid/video`"),
            "the channel is the processor id lowercased, then the port:\n{text}"
        );
        assert!(
            text.contains(&format!(
                "{}-byte frame header",
                streamlib::sdk::iceoryx2::FRAME_HEADER_SIZE
            )),
            "{text}"
        );
    }

    #[tokio::test]
    async fn a_prompt_request_naming_nothing_the_node_has_is_refused_by_name() {
        for (case, prompt_name, arguments, named) in [
            (
                "an unknown prompt",
                "delete_everything",
                json!({}),
                "delete_everything",
            ),
            (
                "a missing required argument",
                "insert_processor_between_linked_processors",
                json!({ "link_id": "link-pattern-to-window" }),
                "processor_type",
            ),
            (
                "a link the graph does not have",
                "insert_processor_between_linked_processors",
                json!({ "link_id": "no-such-link", "processor_type": "effects:Blur" }),
                "no-such-link",
            ),
            (
                "a processor the graph does not have",
                "look_at_what_a_channel_carries",
                json!({ "from_processor_id": "NoSuchProcessor", "from_port": "video" }),
                "NoSuchProcessor",
            ),
            (
                "a port that is not one of the processor's outputs",
                "look_at_what_a_channel_carries",
                json!({ "from_processor_id": "WindowSinkId", "from_port": "video" }),
                "video",
            ),
            (
                "an argument that is not a string",
                "look_at_what_a_channel_carries",
                json!({ "from_processor_id": 7, "from_port": "video" }),
                "from_processor_id",
            ),
        ] {
            let error = rpc_error(
                stub_serving_two_linked_processors(),
                "prompts/get",
                json!({ "name": prompt_name, "arguments": arguments }),
            )
            .await;
            assert_eq!(error["code"], -32602, "{case}: {error}");
            assert!(
                error["message"].as_str().unwrap().contains(named),
                "{case} must be refused naming `{named}`: {error}"
            );
        }
    }

    /// Resources and prompts expose nothing the tools do not, and are gated
    /// exactly as `graph` is: by the one bearer gate in front of `POST /mcp`.
    ///
    /// Paired, because the gate sits on the route: an unauthorised call is
    /// refused whether or not the method exists, so only the authorised half
    /// proves each method is served behind it.
    #[tokio::test]
    async fn resources_and_prompts_answer_behind_the_bearer_gate_and_nowhere_else() {
        use axum::http::header::AUTHORIZATION;
        const TOKEN: &str = "mcp-resources-secret";

        for (method, params) in [
            ("resources/list", json!({})),
            ("resources/read", json!({ "uri": "streamlib://graph" })),
            ("prompts/list", json!({})),
            (
                "prompts/get",
                json!({ "name": "look_at_what_a_channel_carries", "arguments": { "from_processor_id": "PatternSourceId", "from_port": "video" } }),
            ),
        ] {
            let message = json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params })
                .to_string();
            let request_with = |authorization: Option<String>| {
                let mut request = Request::builder()
                    .method("POST")
                    .uri("/mcp")
                    .header(CONTENT_TYPE, "application/json");
                if let Some(authorization) = authorization {
                    request = request.header(AUTHORIZATION, authorization);
                }
                request.body(Body::from(message.clone())).unwrap()
            };
            let router = || {
                crate::handlers::build_router(
                    stub_serving_two_linked_processors(),
                    Some(crate::auth::ApiServerBearerToken::from_secret(TOKEN)),
                )
            };

            let refused = router().oneshot(request_with(None)).await.unwrap();
            assert_eq!(refused.status(), StatusCode::UNAUTHORIZED, "{method}");

            let answered = router()
                .oneshot(request_with(Some(format!("Bearer {TOKEN}"))))
                .await
                .unwrap();
            assert_eq!(answered.status(), StatusCode::OK, "{method}");
            let bytes = axum::body::to_bytes(answered.into_body(), usize::MAX)
                .await
                .unwrap();
            let body: Value = serde_json::from_slice(&bytes).unwrap();
            assert!(
                body["error"].is_null() && !body["result"].is_null(),
                "{method} must answer a result behind the gate: {body}"
            );
        }
    }

    #[tokio::test]
    async fn the_fan_prompt_adds_one_consumer_and_wires_it_to_the_named_port() {
        let text = prompt_text(
            stub_serving_two_linked_processors(),
            "fan_output_to_another_consumer",
            json!({ "from_processor_id": "PatternSourceId", "from_port": "video", "processor_type": "effects:Blur" }),
        )
        .await;

        assert_eq!(
            tool_names_the_numbered_steps_call(&text),
            ["add_processor", "graph", "connect", "graph"]
        );
        assert!(
            text.contains("`from_processor_id`: `PatternSourceId`, `from_port`: `video`"),
            "{text}"
        );
    }

    /// A consumer reading another profile than the ones an output port already
    /// feeds is fanned like any other. Mental revert: restore the refusal and
    /// the recipe is an error naming both profiles.
    #[tokio::test]
    async fn the_fan_prompt_wires_a_consumer_reading_another_profile_than_the_port_already_feeds() {
        let ordered_input_probe = register_a_sole_input_probe_once("ordered");

        let text = prompt_text(
            stub_serving_two_linked_processors(),
            "fan_output_to_another_consumer",
            json!({ "from_processor_id": "PatternSourceId", "from_port": "video", "processor_type": ordered_input_probe }),
        )
        .await;

        assert_eq!(
            tool_names_the_numbered_steps_call(&text),
            ["add_processor", "graph", "connect", "graph"]
        );
    }

    /// The virtual camera's `newest` input joins a port whatever its other
    /// consumers read. Mental revert: restore the refusal and a port already
    /// feeding an `ordered` consumer refuses the recipe.
    #[tokio::test]
    async fn the_virtual_camera_prompt_wires_onto_a_port_that_feeds_an_ordered_consumer() {
        register_a_virtual_camera_sink_probe_once();
        let runtime = ControlPlaneMcpDispatchStubRuntime::new();
        let mut graph = two_linked_processors_graph();
        graph["nodes"][1]["ports"]["inputs"][0]["delivery_profile"] = json!("ordered");
        *runtime.exported_graph.lock() = graph;

        let text = prompt_text(
            Arc::new(runtime),
            "show_channel_on_virtual_camera",
            json!({ "from_processor_id": "PatternSourceId", "from_port": "video" }),
        )
        .await;

        assert_eq!(
            tool_names_the_numbered_steps_call(&text),
            ["add_processor", "connect", "graph"]
        );
    }

    /// A probe with one input declaring `delivery_profile`, registered once per
    /// profile for the test binary.
    fn register_a_sole_input_probe_once(delivery_profile: &'static str) -> String {
        static REGISTERED_PROFILES: Mutex<Vec<&'static str>> = Mutex::new(Vec::new());
        let class_import_path = format!(
            "streamlib_api_server::prompt_delivery_profile_probe::{delivery_profile}::SoleInputProbe"
        );
        let mut registered_profiles = REGISTERED_PROFILES.lock();
        if !registered_profiles.contains(&delivery_profile) {
            PROCESSOR_REGISTRY
                .register_descriptor_only(
                    ProcessorDescriptor::new(
                        ProcessorClassShortName::new("SoleInputProbe").unwrap(),
                        ProcessorClassImportPath::new(&class_import_path).unwrap(),
                        "a probe with one input",
                    )
                    .with_input(
                        PortDescriptor::new("bags_from_upstream", "", true)
                            .with_delivery_profile(delivery_profile),
                    )
                    .with_output(PortDescriptor::new(
                        "bags_to_downstream",
                        "",
                        true,
                    )),
                )
                .expect("each probe path is registered by this helper alone");
            registered_profiles.push(delivery_profile);
        }
        class_import_path
    }

    /// Consumers of one output port read it under whatever profiles they each
    /// declare, so a type reading another profile than the link it is spliced
    /// into still takes the zero-gap order: both new links first, the old one
    /// last. Mental revert: take the link out first again and the target sees a
    /// gap no refusal calls for.
    #[tokio::test]
    async fn inserting_a_type_that_reads_another_profile_than_the_link_connects_before_it_disconnects()
     {
        for (inserted_profile, replaced_profile) in [("newest", "ordered"), ("ordered", "newest")] {
            let inserted_probe = register_a_sole_input_probe_once(inserted_profile);
            let runtime = ControlPlaneMcpDispatchStubRuntime::new();
            let mut graph = two_linked_processors_graph();
            graph["nodes"][1]["ports"]["inputs"][0]["delivery_profile"] = json!(replaced_profile);
            *runtime.exported_graph.lock() = graph;

            let text = prompt_text(
                Arc::new(runtime),
                "insert_processor_between_linked_processors",
                json!({ "link_id": "link-pattern-to-window", "processor_type": inserted_probe }),
            )
            .await;

            assert_eq!(
                tool_names_the_numbered_steps_call(&text),
                [
                    "add_processor",
                    "graph",
                    "connect",
                    "connect",
                    "disconnect",
                    "graph"
                ],
                "a `{inserted_profile}` type into a `{replaced_profile}` link:\n{text}"
            );
        }
    }

    /// A class the agent just wrote is not in the catalog until its first add,
    /// and nothing about the recipe waits on it: the order is the zero-gap one
    /// and no note asks the agent to read a profile before wiring.
    #[tokio::test]
    async fn an_insert_of_an_uncatalogued_type_connects_before_it_disconnects_with_no_profile_note()
    {
        let text = prompt_text(
            stub_serving_two_linked_processors(),
            "insert_processor_between_linked_processors",
            json!({ "link_id": "link-pattern-to-window", "processor_type": "effects:WrittenJustNow" }),
        )
        .await;

        assert_eq!(
            tool_names_the_numbered_steps_call(&text),
            [
                "add_processor",
                "graph",
                "connect",
                "connect",
                "disconnect",
                "graph"
            ]
        );
        assert!(
            !text.contains("delivery profile") && !text.contains("remove_processor"),
            "nothing about the insert depends on the new type's profile:\n{text}"
        );
    }

    #[tokio::test]
    async fn inserting_into_a_link_whose_target_takes_one_inbound_link_removes_the_link_first() {
        let runtime = ControlPlaneMcpDispatchStubRuntime::new();
        let mut graph = two_linked_processors_graph();
        graph["nodes"][1]["ports"]["inputs"][0]["delivery_profile"] = json!("ordered");
        graph["nodes"][1]["ports"]["inputs"][0]["audio_window"] =
            json!({ "resolved_from": "match_device" });
        *runtime.exported_graph.lock() = graph;

        let text = prompt_text(
            Arc::new(runtime),
            "insert_processor_between_linked_processors",
            json!({ "link_id": "link-pattern-to-window", "processor_type": "effects:NeverImported" }),
        )
        .await;

        assert_eq!(
            tool_names_the_numbered_steps_call(&text),
            [
                "add_processor",
                "graph",
                "disconnect",
                "connect",
                "connect",
                "graph"
            ]
        );
        assert!(text.contains("audio window contract"), "{text}");
    }
}
