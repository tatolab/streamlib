// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! HTTP + WebSocket handlers, wired into the router by [`build_router`].

use axum::{
    Json, Router,
    extract::Path,
    extract::Query,
    extract::State,
    extract::ws::{Message, WebSocket, WebSocketUpgrade},
    http::StatusCode,
    http::header::CONTENT_TYPE,
    response::{IntoResponse, Response},
    routing::get,
};
use futures_util::{SinkExt, StreamExt};
use parking_lot::Mutex;
use serde::Deserialize;
use std::sync::Arc;
use streamlib::sdk::descriptors::ProcessorDescriptor;
use streamlib::sdk::error::{Error, Result};
use streamlib::sdk::json_schema::{ProcessorDescriptorOutput, RegistryResponse};
use streamlib::sdk::pubsub::{Event, EventListener, PUBSUB, topics};
use streamlib::sdk::runtime::{OperationsOnTheStreamsLoadedInThisRuntime, RuntimeOperations};
use tokio_util::sync::CancellationToken;
use tower_http::trace::{DefaultMakeSpan, DefaultOnRequest, DefaultOnResponse, TraceLayer};
use tracing::Level;
use utoipa::OpenApi;
use utoipa_axum::{router::OpenApiRouter, routes};

use streamlib_runtime_client_contract::local_api_wire_contract::{
    MCP_STDIO_UPGRADE_REQUEST_TARGET, MCP_STREAMABLE_HTTP_ROUTE_PATH,
    RECYCLED_FRAME_HTTP_STATUS_CODE, SURFACE_PIXEL_HEIGHT_HEADER_NAME,
    SURFACE_PIXEL_WIDTH_HEADER_NAME,
};

use crate::state::{
    ApiDoc, AppState, ErrorResponse, RuntimeShutdownAcceptedResponse, RuntimeShutdownRequest,
    StreamSelectionQuery,
};

// ============================================================================
// Router Construction
// ============================================================================

/// The REST routes the control plane serves, each with its OpenAPI registration.
fn control_plane_rest_routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(health))
        .routes(routes!(get_graph))
        .routes(routes!(get_registry))
        .routes(routes!(request_the_shutdown_of_every_loaded_stream))
        .routes(routes!(exchange_published_surface_id_for_png_image))
}

/// The OpenAPI document for the REST surface, built from the same route
/// registrations `build_router` installs.
///
/// The codegen binary reads the spec through here rather than declaring its own
/// paths: a second inventory drifts silently, and its drift ships in the
/// generated client rather than failing a build.
pub fn control_plane_openapi_spec() -> utoipa::openapi::OpenApi {
    OpenApiRouter::with_openapi(ApiDoc::openapi())
        .merge(control_plane_rest_routes())
        .split_for_parts()
        .1
}

/// Build the full router with shared state and trace layer attached.
///
/// The route surface is observation-shaped: a node's graph is defined by its
/// code, so nothing here creates, replaces, connects, or removes a processor.
/// `POST /api/runtime/shutdown` is the one route that acts on the node rather
/// than reporting on it. No route asks for a credential: whoever can open the
/// local API socket may call every one. `local_api_stopping_token` is
/// cancelled when the local API stops serving, ending every
/// `subscriptions/listen` `/mcp` holds open and every stream `/mcp/stdio`
/// upgraded.
pub(crate) fn build_router(
    operations_on_the_loaded_streams: Arc<dyn OperationsOnTheStreamsLoadedInThisRuntime>,
    local_api_stopping_token: CancellationToken,
) -> Router {
    let (router, openapi) = OpenApiRouter::with_openapi(ApiDoc::openapi())
        .merge(control_plane_rest_routes())
        .split_for_parts();

    let local_api_mcp_server_handler = crate::mcp::LocalApiMcpServerHandler::new(
        Arc::clone(&operations_on_the_loaded_streams),
        local_api_stopping_token.clone(),
    );
    let local_api_mcp_service =
        crate::mcp::local_api_mcp_streamable_http_service(local_api_mcp_server_handler.clone());
    let state = AppState {
        operations_on_the_loaded_streams,
        openapi,
    };

    // Method, path, status and latency for every request, at DEBUG so a client
    // polling the node stays out of the app's own log at the default `info`
    // filter; `RUST_LOG=tower_http=debug` is how you ask for it. The on-failure
    // hook keeps its ERROR default — a request that fails is news either way.
    let trace_layer = TraceLayer::new_for_http()
        .make_span_with(DefaultMakeSpan::new().level(Level::DEBUG))
        .on_request(DefaultOnRequest::new().level(Level::DEBUG))
        .on_response(DefaultOnResponse::new().level(Level::DEBUG));

    let router = router
        .route("/ws/events", get(websocket_handler))
        .route("/api/openapi.json", get(get_openapi_spec))
        .route("/ws/tap/{channel}", get(tap_websocket_handler))
        .route_service(MCP_STREAMABLE_HTTP_ROUTE_PATH, local_api_mcp_service)
        .route(
            MCP_STDIO_UPGRADE_REQUEST_TARGET,
            crate::mcp_stdio_upgrade::local_api_mcp_stdio_upgrade_route(
                local_api_mcp_server_handler,
                local_api_stopping_token,
            ),
        );

    router.layer(trace_layer).with_state(state)
}

// ============================================================================
// API Handlers
// ============================================================================

#[utoipa::path(
    get,
    path = "/health",
    tag = "graph",
    responses(
        (status = 200, description = "Server is healthy", body = String)
    )
)]
pub(crate) async fn health() -> &'static str {
    "ok"
}

/// What a route answers when the stream it was asked about cannot be named:
/// `404` for one not loaded, or an absent `stream` meeting none or several —
/// the refusal names the loaded streams — and `500` for anything else.
fn stream_resolution_refusal_response(refusal: &Error) -> Response {
    let status = match refusal {
        Error::NotFound(_) => StatusCode::NOT_FOUND,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    };
    error_response(status, refusal)
}

/// The operations on the stream `stream` names — `None` names the only
/// loaded stream — or the response refusing the call.
fn stream_operations_or_refusal(
    state: &AppState,
    stream: Option<&str>,
) -> std::result::Result<Arc<dyn RuntimeOperations>, Box<Response>> {
    state
        .operations_on_the_loaded_streams
        .runtime_operations_of_the_stream_a_call_names(stream)
        .map_err(|refusal| Box::new(stream_resolution_refusal_response(&refusal)))
}

/// `status` with `error` as an [`ErrorResponse`] body.
fn error_response(status: StatusCode, error: &impl std::fmt::Display) -> Response {
    (
        status,
        Json(ErrorResponse {
            error: error.to_string(),
        }),
    )
        .into_response()
}

#[utoipa::path(
    get,
    path = "/api/graph",
    tag = "graph",
    params(
        ("stream" = Option<String>, Query, description = "The loaded stream whose graph to export; absent names the only loaded stream")
    ),
    responses(
        (status = 200, description = "The stream's current graph state as JSON"),
        (status = 404, description = "The named stream is not loaded, or `stream` was absent while none or several are; the error names the loaded streams", body = ErrorResponse),
        (status = 500, description = "Internal server error", body = ErrorResponse)
    )
)]
pub(crate) async fn get_graph(
    State(state): State<AppState>,
    Query(StreamSelectionQuery { stream }): Query<StreamSelectionQuery>,
) -> Response {
    let stream_operations = match stream_operations_or_refusal(&state, stream.as_deref()) {
        Ok(stream_operations) => stream_operations,
        Err(refusal) => return *refusal,
    };
    match stream_operations.to_json_async().await {
        Ok(graph) => Json(graph).into_response(),
        Err(export_failure) => error_response(StatusCode::INTERNAL_SERVER_ERROR, &export_failure),
    }
}

#[utoipa::path(
    post,
    path = "/api/runtime/shutdown",
    tag = "runtime",
    request_body = RuntimeShutdownRequest,
    responses(
        (status = 202, description = "Shutdown of every loaded stream requested; teardown proceeds asynchronously and is NOT awaited by this response", body = RuntimeShutdownAcceptedResponse),
        (status = 500, description = "The request could not be handed to the runtime", body = ErrorResponse)
    )
)]
pub(crate) async fn request_the_shutdown_of_every_loaded_stream(
    State(state): State<AppState>,
    Json(body): Json<RuntimeShutdownRequest>,
) -> axum::response::Response {
    let reason = body.reason.unwrap_or_default();

    // Never await teardown: the host stops serving this very socket once every
    // stream has ended, so a handler that waited would be racing its own
    // socket.
    match state
        .operations_on_the_loaded_streams
        .request_the_shutdown_of_every_loaded_stream(&reason)
    {
        Ok(()) => (
            StatusCode::ACCEPTED,
            Json(RuntimeShutdownAcceptedResponse {
                status: crate::state::RUNTIME_SHUTDOWN_REQUESTED_STATUS,
                reason,
            }),
        )
            .into_response(),
        Err(error) => error_response(StatusCode::INTERNAL_SERVER_ERROR, &error),
    }
}

/// [`SURFACE_PIXEL_WIDTH_HEADER_NAME`], parsed once rather than per response:
/// `HeaderName::from_static` panics on a malformed name, and once at first use
/// beats once per 200.
static SURFACE_PIXEL_WIDTH_HEADER: std::sync::LazyLock<axum::http::HeaderName> =
    std::sync::LazyLock::new(|| {
        axum::http::HeaderName::from_static(SURFACE_PIXEL_WIDTH_HEADER_NAME)
    });

/// Height counterpart of [`SURFACE_PIXEL_WIDTH_HEADER`].
static SURFACE_PIXEL_HEIGHT_HEADER: std::sync::LazyLock<axum::http::HeaderName> =
    std::sync::LazyLock::new(|| {
        axum::http::HeaderName::from_static(SURFACE_PIXEL_HEIGHT_HEADER_NAME)
    });

/// Query parameters for the surface exchange.
#[derive(Deserialize)]
pub(crate) struct SurfaceImageExchangeQuery {
    /// Bound the returned image's long edge to this many pixels, aspect
    /// preserved and never upscaled; absent returns the frame at its exact
    /// source resolution.
    downscale_long_edge_pixel_cap: Option<u32>,
}

/// `GET /api/surfaces/{surface_id}/image` — exchange a published surface id
/// for that frame's pixels as a PNG.
///
/// The exact frame, losslessly, with no window in the graph and no display
/// server in the path. A pooled frame id carries a `#<generation>` suffix,
/// so a client percent-encodes it (`slot%237`). The claim on the frame is
/// taken and released inside the operation; this handler only carries
/// bytes.
#[utoipa::path(
    get,
    path = "/api/surfaces/{surface_id}/image",
    tag = "surfaces",
    params(
        ("surface_id" = String, Path, description = "A surface id a bag published; percent-encode the `#` of a `<slot>#<generation>` frame id"),
        ("downscale_long_edge_pixel_cap" = Option<u32>, Query, description = "Bound the image's long edge to this many pixels, aspect preserved and never upscaled; absent returns the exact source resolution")
    ),
    responses(
        (status = 200, description = "The frame as a lossless RGBA8 PNG. `x-streamlib-surface-pixel-width` / `-height` report the surface's own extent, which differs from the image's when a downscale cap applied.", content_type = "image/png"),
        (status = 404, description = "No surface of that id resolves on this node", body = ErrorResponse),
        (status = 410, description = "The id named a frame whose pool slot has since been recycled; tap a newer bag and exchange that", body = ErrorResponse),
        (status = 501, description = "The surface resolves, but its pixel format has no conversion arm in the RHI yet", body = ErrorResponse),
        (status = 500, description = "The frame could not be copied off the GPU", body = ErrorResponse)
    )
)]
pub(crate) async fn exchange_published_surface_id_for_png_image(
    State(state): State<AppState>,
    Path(surface_id): Path<String>,
    Query(query): Query<SurfaceImageExchangeQuery>,
) -> Response {
    match state
        .operations_on_the_loaded_streams
        .exchange_published_surface_id_for_png_image_bytes_async(
            surface_id,
            query.downscale_long_edge_pixel_cap,
        )
        .await
    {
        Ok(exchanged) => (
            StatusCode::OK,
            [
                (CONTENT_TYPE, "image/png".to_string()),
                (
                    SURFACE_PIXEL_WIDTH_HEADER.clone(),
                    exchanged.source_surface_pixel_width.to_string(),
                ),
                (
                    SURFACE_PIXEL_HEIGHT_HEADER.clone(),
                    exchanged.source_surface_pixel_height.to_string(),
                ),
            ],
            exchanged.png_image_bytes,
        )
            .into_response(),
        Err(failure) => surface_exchange_failure_response(&failure),
    }
}

/// [`RECYCLED_FRAME_HTTP_STATUS_CODE`] as the status a refused exchange answers.
const RECYCLED_FRAME_HTTP_STATUS: StatusCode =
    match StatusCode::from_u16(RECYCLED_FRAME_HTTP_STATUS_CODE) {
        Ok(recycled_frame_http_status) => recycled_frame_http_status,
        Err(_) => panic!("the recycled-frame status code is outside HTTP's status range"),
    };

/// Status for a refused exchange.
///
/// A recycled frame is its own answer: the id was well-formed and the
/// frame is gone, so the caller taps a newer bag rather than concluding the
/// surface never existed.
fn surface_exchange_failure_response(failure: &Error) -> Response {
    let status = match failure {
        Error::SurfaceFrameRecycled { .. } => RECYCLED_FRAME_HTTP_STATUS,
        Error::NotFound(_) => StatusCode::NOT_FOUND,
        Error::NotSupported(_) => StatusCode::NOT_IMPLEMENTED,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    };
    error_response(status, failure)
}

#[utoipa::path(
    get,
    path = "/api/registry",
    tag = "registry",
    params(
        ("stream" = Option<String>, Query, description = "The loaded stream whose node catalog to list; absent names the only loaded stream")
    ),
    responses(
        (status = 200, description = "The node types the stream can add: the natively compiled ones, then the ones described in its own interpreter", body = RegistryResponse),
        (status = 404, description = "The named stream is not loaded, or `stream` was absent while none or several are; the error names the loaded streams", body = ErrorResponse)
    )
)]
pub(crate) async fn get_registry(
    State(state): State<AppState>,
    Query(StreamSelectionQuery { stream }): Query<StreamSelectionQuery>,
) -> Response {
    match state
        .operations_on_the_loaded_streams
        .node_catalog_of_the_stream_a_call_names(stream.as_deref())
    {
        Ok(node_catalog) => Json(node_catalog_response(&node_catalog)).into_response(),
        Err(refusal) => stream_resolution_refusal_response(&refusal),
    }
}

/// One stream's node catalog, rendered as `/api/registry` and the MCP node
/// catalog resource both serve it.
pub(crate) fn node_catalog_response(node_catalog: &[ProcessorDescriptor]) -> RegistryResponse {
    RegistryResponse {
        nodes: node_catalog
            .iter()
            .map(ProcessorDescriptorOutput::from)
            .collect(),
    }
}

pub(crate) async fn get_openapi_spec(
    State(state): State<AppState>,
) -> Json<utoipa::openapi::OpenApi> {
    Json(state.openapi)
}

// ============================================================================
// WebSocket Event Streaming
// ============================================================================

pub(crate) async fn websocket_handler(ws: WebSocketUpgrade) -> impl IntoResponse {
    ws.on_upgrade(handle_websocket)
}

async fn handle_websocket(socket: WebSocket) {
    let (mut sender, mut receiver) = socket.split();

    // Channel to bridge sync EventListener -> async WebSocket
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Event>();

    let listener: Arc<Mutex<dyn EventListener>> =
        Arc::new(Mutex::new(WebSocketEventForwarder { tx }));
    // Closing beats serving a client that would receive nothing forever.
    if let Err(subscribe_error) = PUBSUB.subscribe(topics::ALL, Arc::clone(&listener)) {
        tracing::warn!("WebSocket client not subscribed, closing: {subscribe_error}");
        return;
    }

    tracing::info!("WebSocket client connected, subscribed to all events");

    // Task: forward channel events to WebSocket
    let send_task = tokio::spawn(async move {
        while let Some(event) = rx.recv().await {
            match serde_json::to_string(&event) {
                Ok(json) => {
                    if sender.send(Message::Text(json.into())).await.is_err() {
                        break;
                    }
                }
                Err(e) => {
                    tracing::warn!("Failed to serialize event: {}", e);
                }
            }
        }
    });

    // Receive loop (keep-alive, handle close)
    while let Some(msg) = receiver.next().await {
        match msg {
            Ok(Message::Close(_)) => {
                tracing::info!("WebSocket client closed connection");
                break;
            }
            Err(e) => {
                tracing::warn!("WebSocket error: {}", e);
                break;
            }
            _ => {} // axum handles ping/pong automatically
        }
    }

    // The bus removes the subscription at its next publish or subscribe.
    drop(listener);
    send_task.abort();
    tracing::info!("WebSocket client disconnected");
}

struct WebSocketEventForwarder {
    tx: tokio::sync::mpsc::UnboundedSender<Event>,
}

impl EventListener for WebSocketEventForwarder {
    fn on_event(&mut self, event: &Event) -> Result<()> {
        let _ = self.tx.send(event.clone());
        Ok(())
    }
}

// ============================================================================
// Channel Tap WebSocket (read-only channel observer)
// ============================================================================

/// Query parameters for the tap WebSocket: the stream the channel is in and
/// an optional bounded sample count.
#[derive(Deserialize)]
pub(crate) struct TapQuery {
    /// The loaded stream the channel is in; absent names the only loaded one.
    stream: Option<String>,
    /// Stream exactly `count` bags then close; absent streams live until the
    /// client disconnects.
    count: Option<usize>,
}

/// `GET /ws/tap/{channel}` — attach a read-only tap to `channel` and stream its
/// raw bags as binary WebSocket frames.
///
/// Bag bytes are forwarded verbatim (the `FrameHeader`-framed wire form);
/// decoding is the client's concern, which keeps the tap wire-neutral across
/// Rust / Python / Deno publishers. Dropping the connection detaches the tap
/// and frees the channel's reserved slot.
#[utoipa::path(
    get,
    path = "/ws/tap/{channel}",
    tag = "events",
    params(
        ("channel" = String, Path, description = "The output port's address, `<runtime_name>/<node>/<port>`, percent-encoded as one path segment"),
        ("stream" = Option<String>, Query, description = "The loaded stream the channel is in; absent names the only loaded stream"),
        ("count" = Option<usize>, Query, description = "Stream exactly this many bags then close; absent streams live until the client disconnects")
    ),
    responses(
        (status = 404, description = "The named stream is not loaded, or `stream` was absent while none or several are; the error names the loaded streams", body = ErrorResponse),
        (status = 101, description = "WebSocket upgraded. Read-only observability tap: each channel bag is forwarded verbatim (FrameHeader-framed) as a binary WS frame with no encode, containerize, or transcode — decoding is the client's concern. To observe a viewable video feed, tap an encoded (h264/h265/jpeg) or container (CMAF/fMP4) channel; a raw video channel carries zero-copy DMA-BUF/VkImage frame descriptors (meaningless off-host), not pixels, and this is not a realtime-video transport (use the WebRTC/display processors).")
    )
)]
pub(crate) async fn tap_websocket_handler(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    Path(channel): Path<String>,
    Query(query): Query<TapQuery>,
) -> Response {
    let stream_operations = match stream_operations_or_refusal(&state, query.stream.as_deref()) {
        Ok(stream_operations) => stream_operations,
        Err(refusal) => return *refusal,
    };
    ws.on_upgrade(move |socket| {
        handle_tap_websocket(socket, stream_operations, channel, query.count)
    })
}

async fn handle_tap_websocket(
    socket: WebSocket,
    stream_operations: Arc<dyn RuntimeOperations>,
    channel: String,
    count: Option<usize>,
) {
    let (mut sender, mut receiver) = socket.split();

    // Attach the tap; a resolution / slot-occupied failure closes the socket
    // with the typed reason rather than silently hanging.
    let mut subscription = match stream_operations.tap_async(channel.clone(), count).await {
        Ok(subscription) => subscription,
        Err(e) => {
            tracing::info!(channel = %channel, "tap attach rejected: {e}");
            let (close_code, close_reason) = tap_error_close_frame(&e);
            let _ = sender
                .send(Message::Close(Some(axum::extract::ws::CloseFrame {
                    code: close_code,
                    reason: close_reason.into(),
                })))
                .await;
            return;
        }
    };

    tracing::info!(channel = %channel, "tap client attached");

    // Own the subscription in this scope: forward bags until the tap ends
    // (bounded count reached / channel gone) or the client disconnects.
    loop {
        tokio::select! {
            maybe_bag = subscription.recv() => match maybe_bag {
                Some(bytes) => {
                    if sender.send(Message::Binary(bytes.into())).await.is_err() {
                        break;
                    }
                }
                None => {
                    let _ = sender.send(Message::Close(None)).await;
                    break;
                }
            },
            maybe_msg = receiver.next() => match maybe_msg {
                Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
                _ => {}
            },
        }
    }

    // Detach off the async worker: `TapSubscription::drop` joins the forwarder
    // OS thread, and a synchronous join must never run on a tokio runtime
    // worker. The join is bounded (the forwarder never parks), but blocking a
    // shared executor thread on it is still wrong.
    if let Err(join_error) = tokio::task::spawn_blocking(move || drop(subscription)).await {
        tracing::warn!(channel = %channel, "tap detach task failed to join: {join_error}");
    }

    tracing::info!(channel = %channel, "tap client detached");
}

/// Longest tap close reason RFC 6455 permits: a control frame caps its payload
/// at 125 bytes and the 2-byte close code consumes the first two, leaving 123
/// for the UTF-8 reason. tungstenite refuses to write an over-length close
/// frame, so an untruncated tap error string (`NotSupported` runs ~180 bytes)
/// would drop the client into an abnormal close with no reason at all.
const MAX_WS_CLOSE_REASON_BYTES: usize = 123;

/// Map a typed tap error to a WebSocket close code + a short, RFC-6455-legal
/// reason (≤ [`MAX_WS_CLOSE_REASON_BYTES`], truncated on a UTF-8 char
/// boundary). The full error is logged server-side at the call site; this
/// surface is the machine-readable failure the client (and the #1429 MCP tool)
/// reads off the close frame. App codes live in the 4000–4999 private range.
fn tap_error_close_frame(error: &Error) -> (u16, String) {
    let (code, reason) = match error {
        Error::TapChannelNotFound(channel) => (4404, format!("tap channel not found: {channel}")),
        Error::TapSlotOccupied(channel) => (4409, format!("tap slot already occupied: {channel}")),
        other => (
            axum::extract::ws::close_code::ERROR,
            format!("tap attach failed: {other}"),
        ),
    };
    (
        code,
        truncate_on_char_boundary(reason, MAX_WS_CLOSE_REASON_BYTES),
    )
}

/// Truncate `text` to at most `max_bytes`, cutting on a UTF-8 char boundary so
/// the result stays valid UTF-8.
fn truncate_on_char_boundary(mut text: String, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text;
    }
    let mut boundary = max_bytes;
    while boundary > 0 && !text.is_char_boundary(boundary) {
        boundary -= 1;
    }
    text.truncate(boundary);
    text
}

#[cfg(test)]
pub(crate) mod router_surface_tests {
    //! What [`build_router`] exposes.
    //!
    //! The control plane is observation-shaped, so the router must expose no
    //! route that mutates the graph — a node's graph comes from its code. And
    //! no route asks for a credential: file permission on the local API socket
    //! is the whole gate.
    //!
    //! The router is the real one; only the `RuntimeOperations` backend is a
    //! stub.

    use super::*;
    use crate::control_plane_stub_support::{
        STUB_EXCHANGED_FRAME_SURFACE_ID, STUB_EXCHANGED_FRAME_SURFACE_ID_PERCENT_ENCODED,
        STUB_EXCHANGED_IMAGE_BYTES, STUB_SOURCE_SURFACE_EXTENT, StubSurfaceExchange,
    };
    use axum::body::Body;
    use axum::http::{Request, StatusCode, header::CONTENT_TYPE};
    use streamlib::sdk::descriptors::{
        ProcessorClassImportPath, ProcessorClassShortName, ProcessorDescriptor,
    };
    use streamlib::sdk::processors::PROCESSOR_REGISTRY;
    use streamlib::sdk::runtime::BoxFuture;
    use streamlib_runtime_client_contract::local_api_wire_contract::SURFACE_IMAGE_EXCHANGE_ROUTE_PATH_TEMPLATE;
    use tower::ServiceExt;

    /// Stub runtime backing the router tests: it answers the observation ops
    /// and records every shutdown reason it is handed, so a route test can
    /// prove the request reached the runtime rather than merely producing a
    /// 202.
    ///
    /// Every graph-mutating op is `unreachable!`. `RuntimeOperations` still
    /// declares them — the runtime API is not what changed — but no route may
    /// reach one, so a route that regrows here fails loudly instead of quietly
    /// succeeding against a permissive stub.
    #[derive(Clone, Default)]
    struct ControlPlaneRouterStubRuntime {
        recorded_shutdown_reasons: Arc<Mutex<Vec<String>>>,
        exchange: StubSurfaceExchange,
    }

    impl RuntimeOperations for ControlPlaneRouterStubRuntime {
        fn to_json_async(&self) -> BoxFuture<'_, Result<serde_json::Value>> {
            Box::pin(async { Ok(serde_json::json!({})) })
        }
        fn to_json(&self) -> Result<serde_json::Value> {
            Ok(serde_json::json!({}))
        }
        fn tap_async(
            &self,
            channel: String,
            _count: Option<usize>,
        ) -> BoxFuture<'_, Result<streamlib::sdk::runtime::TapSubscription>> {
            Box::pin(async move { Err(Error::TapChannelNotFound(channel)) })
        }

        crate::control_plane_stub_support::graph_mutation_ops_are_unreachable!("route");
    }

    crate::control_plane_stub_support::a_stub_runtime_loading_this_stub_as_its_only_stream!(
        ControlPlaneRouterStubRuntime
    );

    /// The routes this control plane deliberately does not have: every graph
    /// mutation the pre-pivot api-server served. Method + path exactly as they
    /// were, so this reads as the inventory it is.
    const DELETED_GRAPH_MUTATION_ROUTES: &[(&str, &str)] = &[
        ("POST", "/api/processor"),
        ("POST", "/api/processor/source"),
        ("POST", "/api/processor/source/replace"),
        ("DELETE", "/api/processors/some-id"),
        ("POST", "/api/connections"),
        ("DELETE", "/api/connections/some-id"),
    ];

    fn control_plane_router_over(runtime: ControlPlaneRouterStubRuntime) -> Router {
        build_router(Arc::new(runtime), CancellationToken::new())
    }

    /// A default stub runtime, for tests that build the router themselves.
    pub(crate) fn a_control_plane_router_stub_runtime()
    -> Arc<dyn OperationsOnTheStreamsLoadedInThisRuntime> {
        Arc::new(ControlPlaneRouterStubRuntime::default())
    }

    /// The real router over a default stub runtime.
    pub(crate) fn control_plane_router_over_a_stub_runtime() -> Router {
        control_plane_router_over(ControlPlaneRouterStubRuntime::default())
    }

    async fn status_of(request: Request<Body>) -> StatusCode {
        control_plane_router_over_a_stub_runtime()
            .oneshot(request)
            .await
            .unwrap()
            .status()
    }

    fn runtime_shutdown_body() -> Body {
        Body::from(serde_json::json!({ "reason": "operator asked" }).to_string())
    }

    async fn json_body_of(request: Request<Body>) -> serde_json::Value {
        let response = control_plane_router_over_a_stub_runtime()
            .oneshot(request)
            .await
            .unwrap();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    /// The load-bearing surface assertion: no graph-mutation route is served.
    #[tokio::test]
    async fn the_router_serves_no_graph_mutation_route() {
        for (method, uri) in DELETED_GRAPH_MUTATION_ROUTES {
            let request = Request::builder()
                .method(*method)
                .uri(*uri)
                .header(CONTENT_TYPE, "application/json")
                .body(Body::from("{}"))
                .unwrap();
            let status = status_of(request).await;
            assert!(
                status == StatusCode::NOT_FOUND || status == StatusCode::METHOD_NOT_ALLOWED,
                "{method} {uri} must not be routed; got {status}"
            );
        }
    }

    /// The spec is the contract a generated client is built from, so a mutation
    /// route must be absent from it too — not merely unrouted at runtime.
    #[tokio::test]
    async fn the_openapi_spec_documents_no_graph_mutation_route() {
        let request = Request::builder()
            .method("GET")
            .uri("/api/openapi.json")
            .body(Body::empty())
            .unwrap();
        let spec = json_body_of(request).await;
        let paths = &spec["paths"];

        // Positive control. Indexing a `Value` yields `Null` for a missing key
        // AND for indexing a non-object, so the absence assertions below would
        // pass vacuously against an empty or malformed spec.
        assert!(
            paths["/api/graph"]["get"].is_object(),
            "the spec must still document the observation routes: {spec}"
        );

        for (_, uri) in DELETED_GRAPH_MUTATION_ROUTES {
            // The path-templated routes are documented under their template,
            // not the concrete id the runtime check uses.
            let documented = uri.replace("/some-id", "/{id}");
            assert!(
                paths[documented.as_str()].is_null() && paths[*uri].is_null(),
                "{documented} must not appear in the OpenAPI spec"
            );
        }
    }

    /// A generated client names a stream through the `stream` query parameter
    /// of every route that acts on one, so the spec must declare it optional.
    #[tokio::test]
    async fn the_openapi_spec_declares_an_optional_stream_query_on_every_route_acting_on_one_stream()
     {
        let request = Request::builder()
            .method("GET")
            .uri("/api/openapi.json")
            .body(Body::empty())
            .unwrap();
        let spec = json_body_of(request).await;

        for path in ["/api/graph", "/api/registry", "/ws/tap/{channel}"] {
            let parameters = spec["paths"][path]["get"]["parameters"]
                .as_array()
                .unwrap_or_else(|| panic!("{path} must document its parameters: {spec}"));
            let stream_parameter = parameters
                .iter()
                .find(|parameter| parameter["name"] == "stream")
                .unwrap_or_else(|| panic!("{path} must declare `stream`: {parameters:?}"));
            assert_eq!(
                stream_parameter["in"], "query",
                "{path}: {stream_parameter}"
            );
            assert_ne!(
                stream_parameter["required"],
                serde_json::json!(true),
                "{path} must keep `stream` optional: {stream_parameter}"
            );
        }
    }

    /// The stub stream's node catalog is the process-global native registry,
    /// so this registers a probe under a path no other test names and asserts
    /// on that path alone.
    /// There is no teardown: a registration is for the life of the process, and
    /// the registry refuses a second one of the same path.
    ///
    /// `/api/registry` is where an agent learns which keys a node type's
    /// config takes, so it serves the descriptor's schema document itself —
    /// each field's type, its description and its default — rather than a
    /// name the agent would have to look up somewhere the node does not serve.
    #[tokio::test]
    async fn the_registry_serves_a_registered_node_types_config_schema_document() {
        let config_schema = serde_json::json!({
            "type": "object",
            "properties": {
                "width": { "type": "integer", "description": "Frame width in pixels.", "default": 1280 },
                "height": { "type": "integer", "description": "Frame height in pixels.", "default": 720 },
            },
            "required": [],
        });
        let class_import_path = "streamlib_api_server::registry_rendering_probe::TestPatternProbe";
        PROCESSOR_REGISTRY
            .register_descriptor_only(
                ProcessorDescriptor::new(
                    ProcessorClassShortName::new("TestPatternProbe").unwrap(),
                    ProcessorClassImportPath::new(class_import_path).unwrap(),
                    "a registry-rendering probe",
                )
                .with_config_schema(config_schema.clone()),
            )
            .expect("the probe's path is registered by this test alone");

        let request = Request::builder()
            .method("GET")
            .uri("/api/registry")
            .body(Body::empty())
            .unwrap();
        let served = json_body_of(request).await;

        let probe = served["nodes"]
            .as_array()
            .expect("a node type list")
            .iter()
            .find(|entry| entry["type"] == class_import_path)
            .expect("the probe the test registered");
        assert_eq!(probe["config_schema"], config_schema);
    }

    /// The spec a client is generated from and the spec the node serves must be
    /// one document. They were two hand-maintained declarations once; the copy
    /// drifted, kept publishing routes the server had dropped, and nothing went
    /// red because each test read its own side.
    #[tokio::test]
    async fn the_generated_spec_and_the_served_spec_are_the_same_document() {
        let request = Request::builder()
            .method("GET")
            .uri("/api/openapi.json")
            .body(Body::empty())
            .unwrap();
        let served = json_body_of(request).await;
        let generated = serde_json::to_value(control_plane_openapi_spec())
            .expect("the generated spec serializes");
        assert_eq!(
            served, generated,
            "`generate_openapi` and the served `/api/openapi.json` must not diverge"
        );
    }

    #[tokio::test]
    async fn the_observation_routes_answer_ok() {
        for uri in [
            "/health",
            "/api/graph",
            "/api/registry",
            "/api/openapi.json",
        ] {
            let request = Request::builder()
                .method("GET")
                .uri(uri)
                .body(Body::empty())
                .unwrap();
            assert_eq!(
                status_of(request).await,
                StatusCode::OK,
                "GET {uri} must answer 200"
            );
        }
    }

    /// A route reading one stream refuses a stream the runtime does not load
    /// with `404`, naming the streams it does.
    #[tokio::test]
    async fn a_route_naming_a_stream_not_loaded_is_refused_naming_the_loaded_ones() {
        for uri in [
            "/api/graph?stream=elsewhere",
            "/api/registry?stream=elsewhere",
        ] {
            let response = control_plane_router_over_a_stub_runtime()
                .oneshot(
                    Request::builder()
                        .method("GET")
                        .uri(uri)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::NOT_FOUND, "GET {uri}");
            let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap();
            let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            let refusal = body["error"].as_str().unwrap_or_default();
            assert!(
                refusal.contains("elsewhere")
                    && refusal.contains(crate::control_plane_stub_support::STUB_STREAM_NAME),
                "GET {uri}: {refusal}"
            );
        }
    }

    /// A tap naming a stream the runtime does not load is refused before the
    /// upgrade, rather than upgraded and closed.
    #[tokio::test]
    async fn a_tap_naming_a_stream_not_loaded_is_refused_before_the_upgrade() {
        let served = crate::control_plane_stub_support::LocalApiServedOnAFreshSocket::over(
            a_control_plane_router_stub_runtime(),
        );
        let mut connection = tokio::net::UnixStream::connect(&served.local_api_socket_path)
            .await
            .unwrap();
        let head = crate::control_plane_stub_support::response_head_over_the_socket(
            &mut connection,
            "GET /ws/tap/stub-runtime%2Fnode%2Fport?stream=elsewhere HTTP/1.1\r\n\
             Host: localhost\r\nConnection: Upgrade\r\nUpgrade: websocket\r\n\
             Sec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n",
        )
        .await;

        assert!(head.starts_with("HTTP/1.1 404 "), "{head}");
    }

    /// File permission on the local API socket is the whole gate, so a request
    /// carrying no credential reaches every route — the one that acts on the
    /// node included — rather than a 401 or 403. `/mcp` is reached the same
    /// way by every MCP wire test's client.
    #[tokio::test]
    async fn no_route_asks_for_a_credential() {
        let requests_carrying_no_credential = [
            ("POST", "/api/runtime/shutdown".to_string(), "{}"),
            (
                "GET",
                exchange_uri(STUB_EXCHANGED_FRAME_SURFACE_ID_PERCENT_ENCODED),
                "",
            ),
            ("GET", "/ws/tap/some-channel".to_string(), ""),
        ];
        for (method, uri, body) in requests_carrying_no_credential {
            let request = Request::builder()
                .method(method)
                .uri(&uri)
                .header(CONTENT_TYPE, "application/json")
                .body(Body::from(body))
                .unwrap();
            let status = status_of(request).await;
            assert!(
                ![
                    StatusCode::UNAUTHORIZED,
                    StatusCode::FORBIDDEN,
                    StatusCode::NOT_FOUND
                ]
                .contains(&status),
                "{method} {uri} must reach its handler with no credential; got {status}"
            );
        }
    }

    /// The shutdown request must reach the runtime handle — a 202 alone would
    /// also be produced by a handler that dropped the request on the floor —
    /// and it must answer 202 (accepted), never 200, because teardown is not
    /// awaited.
    #[tokio::test]
    async fn runtime_shutdown_is_202_and_reaches_the_runtime() {
        let runtime = Arc::new(ControlPlaneRouterStubRuntime::default());
        let recorded = runtime.recorded_shutdown_reasons.clone();
        let router = build_router(runtime, CancellationToken::new());
        let request = Request::builder()
            .method("POST")
            .uri("/api/runtime/shutdown")
            .header(CONTENT_TYPE, "application/json")
            .body(runtime_shutdown_body())
            .unwrap();

        let response = router.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::ACCEPTED);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["status"], "RuntimeShutdownRequested");
        assert_eq!(body["reason"], "operator asked");
        assert_eq!(
            *recorded.lock(),
            vec!["operator asked".to_string()],
            "the route must hand the reason to the runtime's shutdown funnel"
        );
    }

    /// An omitted `reason` is unspecified, not a 400 — the request is the
    /// point, the attribution is a courtesy.
    #[tokio::test]
    async fn runtime_shutdown_without_a_reason_is_accepted_as_unspecified() {
        let runtime = Arc::new(ControlPlaneRouterStubRuntime::default());
        let recorded = runtime.recorded_shutdown_reasons.clone();
        let router = build_router(runtime, CancellationToken::new());
        let request = Request::builder()
            .method("POST")
            .uri("/api/runtime/shutdown")
            .header(CONTENT_TYPE, "application/json")
            .body(Body::from("{}"))
            .unwrap();

        assert_eq!(
            router.oneshot(request).await.unwrap().status(),
            StatusCode::ACCEPTED
        );
        assert_eq!(*recorded.lock(), vec![String::new()]);
    }

    // ------------------------------------------------------------------
    // Surface exchange: a published surface id in, image bytes out
    // ------------------------------------------------------------------

    fn exchange_request(uri: &str) -> Request<Body> {
        Request::builder()
            .method("GET")
            .uri(uri)
            .body(Body::empty())
            .unwrap()
    }

    fn exchange_uri(surface_id: &str) -> String {
        format!("/api/surfaces/{surface_id}/image")
    }

    /// The route's whole job: hand the operation the decoded surface id, and
    /// carry its bytes back verbatim under `image/png`, with the surface's
    /// own extent stated alongside.
    #[tokio::test]
    async fn the_exchange_route_answers_the_operation_bytes_verbatim_as_an_image() {
        let runtime = ControlPlaneRouterStubRuntime::default();
        let recorded = runtime.exchange.recorded_calls.clone();
        let response = control_plane_router_over(runtime)
            .oneshot(exchange_request(&exchange_uri(
                STUB_EXCHANGED_FRAME_SURFACE_ID_PERCENT_ENCODED,
            )))
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get(CONTENT_TYPE).unwrap(),
            "image/png",
            "the body is an image, not a JSON envelope carrying one"
        );
        let (source_pixel_width, source_pixel_height) = STUB_SOURCE_SURFACE_EXTENT;
        assert_eq!(
            response
                .headers()
                .get(&*SURFACE_PIXEL_WIDTH_HEADER)
                .and_then(|value| value.to_str().ok()),
            Some(source_pixel_width.to_string().as_str()),
        );
        assert_eq!(
            response
                .headers()
                .get(&*SURFACE_PIXEL_HEIGHT_HEADER)
                .and_then(|value| value.to_str().ok()),
            Some(source_pixel_height.to_string().as_str()),
        );

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(
            body.as_ref(),
            STUB_EXCHANGED_IMAGE_BYTES,
            "the route re-encodes nothing"
        );
        assert_eq!(
            *recorded.lock(),
            vec![(STUB_EXCHANGED_FRAME_SURFACE_ID.to_string(), None)],
            "the operation must be handed the decoded frame id and no cap"
        );
    }

    /// The cap is the MCP spelling's dial, but it reaches the operation
    /// through the same argument whichever front end spends it.
    #[tokio::test]
    async fn the_downscale_cap_reaches_the_operation_from_the_query_string() {
        let runtime = ControlPlaneRouterStubRuntime::default();
        let recorded = runtime.exchange.recorded_calls.clone();
        let status = control_plane_router_over(runtime)
            .oneshot(exchange_request(&format!(
                "{}?downscale_long_edge_pixel_cap=1568",
                exchange_uri(STUB_EXCHANGED_FRAME_SURFACE_ID_PERCENT_ENCODED)
            )))
            .await
            .unwrap()
            .status();

        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            *recorded.lock(),
            vec![(STUB_EXCHANGED_FRAME_SURFACE_ID.to_string(), Some(1568))]
        );
    }

    /// A recycled frame is `410 Gone`, not `404`: the id was well-formed and
    /// the frame existed, so the caller taps a newer bag and exchanges that
    /// rather than concluding the surface never existed. Never `200` with
    /// the slot's newer pixels.
    #[tokio::test]
    async fn a_recycled_frame_id_is_gone_and_the_body_names_the_recycling() {
        let runtime = ControlPlaneRouterStubRuntime {
            exchange: StubSurfaceExchange::refusing_as_recycled(STUB_EXCHANGED_FRAME_SURFACE_ID),
            ..ControlPlaneRouterStubRuntime::default()
        };
        let response = control_plane_router_over(runtime)
            .oneshot(exchange_request(&exchange_uri(
                STUB_EXCHANGED_FRAME_SURFACE_ID_PERCENT_ENCODED,
            )))
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::GONE);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let reported = body["error"].as_str().unwrap_or_default();
        assert!(
            reported.contains(STUB_EXCHANGED_FRAME_SURFACE_ID),
            "the refusal must name the id asked for: {reported}"
        );
    }

    /// The exchange is part of the documented surface, so a generated client
    /// can reach it without hand-written paths.
    #[test]
    fn the_openapi_spec_documents_the_exchange_route_as_an_image_response() {
        let spec = control_plane_openapi_spec();
        let path = spec
            .paths
            .paths
            .get(SURFACE_IMAGE_EXCHANGE_ROUTE_PATH_TEMPLATE)
            .expect("the exchange route is in the spec, at the path the MCP tool names");
        let operation = path.get.as_ref().expect("it is a GET");
        let ok = operation
            .responses
            .responses
            .get("200")
            .expect("it documents a 200");
        let rendered = serde_json::to_string(ok).expect("the 200 response serializes");
        assert!(
            rendered.contains("image/png"),
            "the 200 must be documented as binary PNG: {rendered}"
        );
    }
}

#[cfg(test)]
mod control_plane_request_trace_level_tests {
    //! The log level a routine control-plane request speaks at.
    //!
    //! At the engine's default `info` filter a node's control plane adds
    //! nothing to the app's own log. The trace is levelled, not deleted, so
    //! `RUST_LOG=tower_http=debug` brings all three records back.

    use super::router_surface_tests::control_plane_router_over_a_stub_runtime;
    use super::*;
    use crate::control_plane_stub_support::CapturedTracingTargets;
    use axum::body::Body;
    use axum::http::Request;
    use serial_test::serial;
    use tower::ServiceExt;

    fn serve_one_graph_request() {
        let request_runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("a current-thread runtime builds");
        request_runtime.block_on(async {
            let response = control_plane_router_over_a_stub_runtime()
                .oneshot(
                    Request::builder()
                        .uri("/api/graph")
                        .body(Body::empty())
                        .expect("the graph request builds"),
                )
                .await
                .expect("the router answers");
            assert_eq!(response.status(), StatusCode::OK);
        });
    }

    fn tower_http_trace_targets_under(env_filter_directives: &str) -> Vec<&'static str> {
        CapturedTracingTargets::captured_from_the_second_of_two_runs(
            env_filter_directives,
            serve_one_graph_request,
        )
        .into_iter()
        .filter(|target| target.starts_with("tower_http"))
        .collect()
    }

    #[test]
    #[serial]
    fn a_routine_request_says_nothing_at_the_default_info_filter() {
        let targets = tower_http_trace_targets_under("info");
        assert!(
            targets.is_empty(),
            "a request must be silent at the engine's default filter, got: {targets:?}"
        );
    }

    #[test]
    #[serial]
    fn the_same_request_traces_all_three_hooks_under_tower_http_debug() {
        let targets = tower_http_trace_targets_under("info,tower_http=debug");
        for hook in [
            "tower_http::trace::make_span",
            "tower_http::trace::on_request",
            "tower_http::trace::on_response",
        ] {
            assert!(
                targets.contains(&hook),
                "asking for the request trace must yield {hook}, got: {targets:?}"
            );
        }
    }
}
