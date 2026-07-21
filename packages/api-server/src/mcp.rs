// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Model Context Protocol (MCP) veneer over the api-server's control-plane ops.
//!
//! A single Streamable-HTTP endpoint (`POST /mcp`) speaks MCP's JSON-RPC 2.0
//! wire directly on the existing axum stack — no separate proxy process, and
//! the same [`crate::auth`] bearer middleware and [`crate::state::AppState`] the
//! REST routes use. It exposes the runtime graph as MCP *tools* so an LLM agent
//! can inspect and mutate the live graph the same way the REST client does; the
//! tool handlers call the shared [`crate::ops`] layer, so the MCP surface and
//! the REST surface can never drift.
//!
//! Two of the tools (`tap`, `logs`) front WebSocket *streams* in the REST API.
//! MCP tools are request/response, so each bridges its stream to a **bounded
//! sample** — `tap` by a bag count, `logs` by a count and a monotonic sample
//! window — and returns the collected sample as the tool result.

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
use streamlib::sdk::graph::{InputLinkPortRef, OutputLinkPortRef};
use streamlib::sdk::pubsub::{Event, EventListener, PUBSUB, topics};
use streamlib::sdk::runtime::SubmittedProcessorSource;

use crate::ops::{ReplaceSourceError, SubmitSourceError, SubmittedSourceOutcome};
use crate::state::{
    AppState, CreateConnectionRequest, ReplaceProcessorSourceRequest,
    SubmittedProcessorSourceRequest,
};

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

// ============================================================================
// JSON-RPC envelope
// ============================================================================

/// An inbound MCP message. A *request* carries an `id` and expects a paired
/// response; a *notification* (e.g. `notifications/initialized`) omits `id` and
/// is acknowledged with `202 Accepted` and no body.
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
/// message and answers with a single `application/json` response (this server's
/// tools are all request/response, so it never opens an SSE stream).
#[tracing::instrument(skip_all, fields(mcp_method = %request.method))]
pub(crate) async fn mcp_endpoint(
    State(state): State<AppState>,
    Json(request): Json<JsonRpcRequest>,
) -> Response {
    let Some(id) = request.id.clone() else {
        // A notification expects no response body under Streamable HTTP.
        return StatusCode::ACCEPTED.into_response();
    };

    let params = request.params.clone().unwrap_or(Value::Null);
    match dispatch(&state, &request.method, params).await {
        Ok(result) => Json(json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": result,
        }))
        .into_response(),
        Err(error) => Json(json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": { "code": error.code, "message": error.message },
        }))
        .into_response(),
    }
}

async fn dispatch(
    state: &AppState,
    method: &str,
    params: Value,
) -> std::result::Result<Value, RpcError> {
    match method {
        "initialize" => Ok(initialize_result()),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({ "tools": tool_definitions() })),
        "tools/call" => tools_call(state, params).await,
        other => Err(RpcError::method_not_found(other)),
    }
}

fn initialize_result() -> Value {
    json!({
        "protocolVersion": MCP_PROTOCOL_VERSION,
        "capabilities": { "tools": { "listChanged": false } },
        "serverInfo": { "name": MCP_SERVER_NAME, "version": MCP_SERVER_VERSION },
        "instructions": "StreamLib runtime control plane. Tools inspect and mutate the live processor graph and observe its channels and event stream.",
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
            "name": "submit_processor",
            "description": "Register a processor from source text, instantiate the first discovered processor, and optionally wire it to existing graph ports. Transactional: a failed wiring rolls the whole submit back.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "language": { "type": "string", "enum": ["rust", "python", "typescript", "deno"], "description": "Source language. `deno` is an alias for `typescript`." },
                    "source": { "type": "string", "description": "The processor module source text." },
                    "requested_name": { "type": "string", "description": "The @session/<name> package segment to mint under. Omit to derive from processor_type_name." },
                    "processor_type_name": { "type": "string", "description": "The PascalCase processor type name the source defines. Omit to derive from requested_name." },
                    "config": { "type": "object", "description": "Config applied when the processor is instantiated. Defaults to {}." },
                    "connect": {
                        "type": "array",
                        "description": "Optional wirings applied after instantiation.",
                        "items": {
                            "type": "object",
                            "properties": {
                                "local_port": { "type": "string" },
                                "role": { "type": "string", "enum": ["output", "input"] },
                                "peer_processor": { "type": "string" },
                                "peer_port": { "type": "string" }
                            },
                            "required": ["local_port", "role", "peer_processor", "peer_port"]
                        }
                    }
                },
                "required": ["language", "source"]
            },
        }),
        json!({
            "name": "replace_processor",
            "description": "Swap a live @session/<name> source registration for a replacement (type-level; running instances are not swapped). Transactional: a failed replacement restores the prior registration.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "target_session_module": { "type": "string", "description": "The @session/<name>@<range> module to replace, e.g. @session/widget@*." },
                    "language": { "type": "string", "enum": ["rust", "python", "typescript", "deno"] },
                    "source": { "type": "string" },
                    "requested_name": { "type": "string" },
                    "processor_type_name": { "type": "string" }
                },
                "required": ["target_session_module", "language", "source"]
            },
        }),
        json!({
            "name": "remove_processor",
            "description": "Remove a processor instance from the graph by id.",
            "inputSchema": {
                "type": "object",
                "properties": { "processor_id": { "type": "string" } },
                "required": ["processor_id"],
                "additionalProperties": false
            },
        }),
        json!({
            "name": "connect",
            "description": "Connect an output port to an input port between two existing processors.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "from_processor": { "type": "string" },
                    "from_port": { "type": "string" },
                    "to_processor": { "type": "string" },
                    "to_port": { "type": "string" }
                },
                "required": ["from_processor", "from_port", "to_processor", "to_port"],
                "additionalProperties": false
            },
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
    ]
}

// ============================================================================
// tools/call dispatch
// ============================================================================

async fn tools_call(state: &AppState, params: Value) -> std::result::Result<Value, RpcError> {
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
        "graph" => call_graph(state).await,
        "submit_processor" => call_submit_processor(state, arguments).await,
        "replace_processor" => call_replace_processor(state, arguments).await,
        "remove_processor" => call_remove_processor(state, arguments).await,
        "connect" => call_connect(state, arguments).await,
        "tap" => call_tap(state, arguments).await,
        "logs" => call_logs(state, arguments).await,
        other => tool_error(format!("unknown tool: {other}")),
    };
    Ok(result)
}

async fn call_graph(state: &AppState) -> Value {
    match state.runtime.to_json_async().await {
        Ok(graph) => tool_ok(graph),
        Err(e) => tool_error(format!("graph export failed: {e}")),
    }
}

async fn call_submit_processor(state: &AppState, arguments: Value) -> Value {
    let request: SubmittedProcessorSourceRequest = match serde_json::from_value(arguments) {
        Ok(request) => request,
        Err(e) => return tool_error(format!("submit_processor arguments: {e}")),
    };
    let submitted = SubmittedProcessorSource {
        source_text: request.source,
        language: request.language.into(),
        requested_name: request.requested_name,
        processor_type_name: request.processor_type_name,
    };
    let config = request.config.unwrap_or_else(|| json!({}));
    match crate::ops::submit_processor_source(&state.runtime, submitted, config, request.connect)
        .await
    {
        Ok(outcome) => tool_ok(submitted_source_json(outcome)),
        Err(error) => tool_error(submit_source_error_message(error)),
    }
}

async fn call_replace_processor(state: &AppState, arguments: Value) -> Value {
    let request: ReplaceProcessorSourceRequest = match serde_json::from_value(arguments) {
        Ok(request) => request,
        Err(e) => return tool_error(format!("replace_processor arguments: {e}")),
    };
    let replacement = SubmittedProcessorSource {
        source_text: request.source,
        language: request.language.into(),
        requested_name: request.requested_name,
        processor_type_name: request.processor_type_name,
    };
    match crate::ops::replace_processor_source(
        &state.runtime,
        &request.target_session_module,
        replacement,
    )
    .await
    {
        Ok(outcome) => tool_ok(submitted_source_json(outcome)),
        Err(ReplaceSourceError::MalformedTargetModule(message)) => tool_error(message),
        Err(ReplaceSourceError::Replace(error)) => tool_error(error.to_string()),
    }
}

async fn call_remove_processor(state: &AppState, arguments: Value) -> Value {
    #[derive(Deserialize)]
    struct RemoveArgs {
        processor_id: String,
    }
    let RemoveArgs { processor_id } = match serde_json::from_value(arguments) {
        Ok(args) => args,
        Err(e) => return tool_error(format!("remove_processor arguments: {e}")),
    };
    match state
        .runtime
        .remove_processor_async(processor_id.clone().into())
        .await
    {
        Ok(()) => tool_ok(json!({ "removed": processor_id })),
        Err(e) => tool_error(format!("remove_processor failed: {e}")),
    }
}

async fn call_connect(state: &AppState, arguments: Value) -> Value {
    let request: CreateConnectionRequest = match serde_json::from_value(arguments) {
        Ok(request) => request,
        Err(e) => return tool_error(format!("connect arguments: {e}")),
    };
    let from = OutputLinkPortRef::new(request.from_processor, request.from_port);
    let to = InputLinkPortRef::new(request.to_processor, request.to_port);
    match state.runtime.connect_async(from, to).await {
        Ok(link_id) => tool_ok(json!({ "link_id": link_id.to_string() })),
        Err(e) => tool_error(format!("connect failed: {e}")),
    }
}

async fn call_tap(state: &AppState, arguments: Value) -> Value {
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

    let mut subscription = match state.runtime.tap_async(channel.clone(), Some(sample)).await {
        Ok(subscription) => subscription,
        Err(e) => return tool_error(format!("tap attach failed: {e}")),
    };

    let mut bags: Vec<Value> = Vec::with_capacity(sample);
    while bags.len() < sample {
        match subscription.recv().await {
            Some(bytes) => bags.push(tap_bag_json(&bytes)),
            None => break,
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
        "dropped_bags": dropped_bags,
        "bags": bags,
    }))
}

async fn call_logs(state: &AppState, arguments: Value) -> Value {
    let _ = state;
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
    PUBSUB.subscribe(topics::ALL, listener.clone());

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

fn submitted_source_json(outcome: SubmittedSourceOutcome) -> Value {
    json!({
        "module": outcome.module,
        "processors": outcome.processors,
        "processor_id": outcome.processor_id,
        "state": outcome.state,
        "connections": outcome.connections,
    })
}

fn submit_source_error_message(error: SubmitSourceError) -> String {
    match error {
        SubmitSourceError::Register(e)
        | SubmitSourceError::Instantiate(e)
        | SubmitSourceError::Connect(e) => e.to_string(),
        SubmitSourceError::Unprocessable(message) => message,
    }
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
    //! handshake, the tool catalog, and a processor submit through to the
    //! runtime — the #1429 acceptance path ("an MCP client lists tools and
    //! submits a processor end-to-end"). The router is the real one; only the
    //! `RuntimeOperations` backend is a stub, so the MCP → [`crate::ops`] →
    //! runtime seam is what's under test.

    use axum::Router;
    use axum::body::Body;
    use axum::http::{Request, StatusCode, header::CONTENT_TYPE};
    use streamlib::sdk::descriptors::{
        ModuleIdent, Org, Package, SchemaIdent, SemVer, SemVerRange, TypeName,
    };
    use streamlib::sdk::error::Error;
    use streamlib::sdk::graph::{
        InputLinkPortRef, LinkUniqueId, OutputLinkPortRef, ProcessorUniqueId,
    };
    use streamlib::sdk::processors::{PortSchemaSpec, ProcessorSpec};
    use streamlib::sdk::runtime::{
        BoxFuture, RegisterProcessorReceipt, RegisteredPortReceipt, RegisteredProcessorReceipt,
        ReplaceProcessorFromSource, RuntimeOperations, SubmittedProcessorSource, TapSubscription,
    };
    use tower::ServiceExt;

    use super::*;

    /// Stub runtime that records the last submitted source and answers every op
    /// with a fixed success, so the MCP tool → ops → runtime path can be
    /// observed end-to-end without a live engine.
    struct RecordingStubRuntime {
        last_submitted_source: Arc<Mutex<Option<String>>>,
        instance_id: ProcessorUniqueId,
    }

    impl RecordingStubRuntime {
        fn new() -> Self {
            Self {
                last_submitted_source: Arc::new(Mutex::new(None)),
                instance_id: "mcp-instance".to_string().into(),
            }
        }
    }

    fn stub_register_receipt() -> RegisterProcessorReceipt {
        RegisterProcessorReceipt::new(
            ModuleIdent::new(
                Org::new("session").unwrap(),
                Package::new("widget").unwrap(),
                SemVerRange::Exact(SemVer::new(0, 0, 0)),
            ),
            vec![RegisteredProcessorReceipt {
                name: "Widget".to_string(),
                inputs: vec![RegisteredPortReceipt {
                    name: "video".to_string(),
                    schema: PortSchemaSpec::Any,
                    delivery_profile: Some("latest".to_string()),
                }],
                outputs: vec![RegisteredPortReceipt {
                    name: "frame".to_string(),
                    schema: PortSchemaSpec::Specific(SchemaIdent::new(
                        Org::new("tatolab").unwrap(),
                        Package::new("core").unwrap(),
                        TypeName::new("VideoFrame").unwrap(),
                        SemVer::new(1, 0, 0),
                    )),
                    delivery_profile: None,
                }],
            }],
        )
    }

    impl RuntimeOperations for RecordingStubRuntime {
        fn add_processor_async(
            &self,
            _spec: ProcessorSpec,
        ) -> BoxFuture<'_, Result<ProcessorUniqueId>> {
            let id = self.instance_id.clone();
            Box::pin(async move { Ok(id) })
        }
        fn remove_processor_async(&self, _id: ProcessorUniqueId) -> BoxFuture<'_, Result<()>> {
            Box::pin(async { Ok(()) })
        }
        fn connect_async(
            &self,
            _from: OutputLinkPortRef,
            _to: InputLinkPortRef,
        ) -> BoxFuture<'_, Result<LinkUniqueId>> {
            Box::pin(async { Ok("mcp-link".to_string().into()) })
        }
        fn disconnect_async(&self, _link_id: LinkUniqueId) -> BoxFuture<'_, Result<()>> {
            Box::pin(async { Ok(()) })
        }
        fn to_json_async(&self) -> BoxFuture<'_, Result<Value>> {
            Box::pin(async { Ok(json!({ "processors": [], "links": [] })) })
        }
        fn register_processor_source_async(
            &self,
            request: SubmittedProcessorSource,
        ) -> BoxFuture<'_, Result<RegisterProcessorReceipt>> {
            *self.last_submitted_source.lock() = Some(request.source_text);
            Box::pin(async { Ok(stub_register_receipt()) })
        }
        fn replace_processor_async(
            &self,
            _request: ReplaceProcessorFromSource,
        ) -> BoxFuture<'_, Result<RegisterProcessorReceipt>> {
            Box::pin(async { Ok(stub_register_receipt()) })
        }
        fn tap_async(
            &self,
            channel: String,
            _count: Option<usize>,
        ) -> BoxFuture<'_, Result<TapSubscription>> {
            Box::pin(async move { Err(Error::TapChannelNotFound(channel)) })
        }
        fn add_processor(&self, _spec: ProcessorSpec) -> Result<ProcessorUniqueId> {
            Ok(self.instance_id.clone())
        }
        fn remove_processor(&self, _id: &ProcessorUniqueId) -> Result<()> {
            Ok(())
        }
        fn connect(&self, _from: OutputLinkPortRef, _to: InputLinkPortRef) -> Result<LinkUniqueId> {
            Ok("mcp-link".to_string().into())
        }
        fn disconnect(&self, _link_id: &LinkUniqueId) -> Result<()> {
            Ok(())
        }
        fn to_json(&self) -> Result<Value> {
            Ok(json!({}))
        }
    }

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
            Arc::new(RecordingStubRuntime::new()),
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
            Arc::new(RecordingStubRuntime::new()),
            json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
        )
        .await;
        assert_eq!(status, StatusCode::ACCEPTED);
        assert_eq!(body, Value::Null);
    }

    #[tokio::test]
    async fn tools_list_advertises_every_veneer_tool() {
        let (status, body) = mcp_call(
            Arc::new(RecordingStubRuntime::new()),
            json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        let tools = body["result"]["tools"].as_array().expect("tools array");
        let names: Vec<&str> = tools
            .iter()
            .filter_map(|tool| tool["name"].as_str())
            .collect();
        for expected in [
            "graph",
            "submit_processor",
            "replace_processor",
            "remove_processor",
            "connect",
            "tap",
            "logs",
        ] {
            assert!(
                names.contains(&expected),
                "tools/list must advertise `{expected}`, got {names:?}"
            );
        }
        for tool in tools {
            assert_eq!(
                tool["inputSchema"]["type"], "object",
                "tool `{}` must declare an object inputSchema",
                tool["name"]
            );
        }
    }

    #[tokio::test]
    async fn tools_call_submit_processor_reaches_the_runtime() {
        let runtime = Arc::new(RecordingStubRuntime::new());
        let recorded = runtime.last_submitted_source.clone();

        let (status, body) = mcp_call(
            runtime,
            json!({
                "jsonrpc": "2.0", "id": 3, "method": "tools/call",
                "params": {
                    "name": "submit_processor",
                    "arguments": {
                        "language": "python",
                        "source": "class Widget:\n    pass\n",
                        "requested_name": "widget"
                    }
                }
            }),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        let result = &body["result"];
        assert_eq!(
            result["isError"], false,
            "a successful submit must not be flagged isError; body={body}"
        );

        let text = result["content"][0]["text"]
            .as_str()
            .expect("tool result text content block");
        let outcome: Value = serde_json::from_str(text).expect("tool text is JSON");
        assert_eq!(outcome["module"], "@session/widget@=0.0.0");
        assert_eq!(outcome["state"], "added");
        assert_eq!(outcome["processor_id"], "mcp-instance");
        assert_eq!(outcome["processors"][0]["name"], "Widget");

        assert_eq!(
            recorded.lock().as_deref(),
            Some("class Widget:\n    pass\n"),
            "the submitted source text must have reached register_processor_source_async"
        );
    }

    #[tokio::test]
    async fn tools_call_graph_returns_the_runtime_json() {
        let (status, body) = mcp_call(
            Arc::new(RecordingStubRuntime::new()),
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
            Arc::new(RecordingStubRuntime::new()),
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
                Arc::new(RecordingStubRuntime::new()),
                Some(crate::auth::ApiServerBearerToken::from_secret(TOKEN)),
                #[cfg(feature = "moq")]
                "test-runtime-id".to_string(),
            )
        };
        let message = json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" }).to_string();

        // No bearer token → the mutating-parity gate rejects with 401 before the
        // JSON-RPC handler runs. Deleting the mcp_router `.route_layer(...)`
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
            Arc::new(RecordingStubRuntime::new()),
            json!({ "jsonrpc": "2.0", "id": 6, "method": "no_such_method" }),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["error"]["code"], -32601);
    }
}
