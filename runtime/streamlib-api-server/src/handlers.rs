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
    routing::{get, post},
};
use futures_util::{SinkExt, StreamExt};
use parking_lot::Mutex;
use serde::Deserialize;
use std::sync::Arc;
use streamlib::sdk::error::{Error, Result};
use streamlib::sdk::json_schema::{ProcessorDescriptorOutput, RegistryResponse};
use streamlib::sdk::processors::PROCESSOR_REGISTRY;
use streamlib::sdk::pubsub::{Event, EventListener, PUBSUB, topics};
use streamlib::sdk::runtime::RuntimeOperations;
use tower_http::trace::{DefaultMakeSpan, DefaultOnRequest, DefaultOnResponse, TraceLayer};
use tracing::Level;
use utoipa::OpenApi;
use utoipa_axum::{router::OpenApiRouter, routes};

use crate::auth::{ApiServerBearerToken, ForbiddenResponse, UnauthorizedResponse};
use crate::state::{
    ApiDoc, AppState, ErrorResponse, RuntimeShutdownAcceptedResponse, RuntimeShutdownRequest,
};

// ============================================================================
// Router Construction
// ============================================================================

/// The REST routes that are open regardless of the auth posture.
fn always_open_routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(health))
        .routes(routes!(get_graph))
        .routes(routes!(get_registry))
}

/// The REST routes the bearer middleware covers when auth is opted in.
fn bearer_gated_routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(request_runtime_shutdown))
        .routes(routes!(exchange_published_surface_id_for_png_image))
}

/// The OpenAPI document for the REST surface, built from the same two route
/// registrations `build_router` installs.
///
/// The codegen binary reads the spec through here rather than declaring its own
/// paths: a second inventory drifts silently, and its drift ships in the
/// generated client rather than failing a build.
pub fn control_plane_openapi_spec() -> utoipa::openapi::OpenApi {
    OpenApiRouter::with_openapi(ApiDoc::openapi())
        .merge(always_open_routes())
        .merge(bearer_gated_routes())
        .split_for_parts()
        .1
}

/// Build the full router with shared state and trace layer attached.
///
/// The route surface is observation-shaped: a node's graph is defined by its
/// code, so nothing here creates, replaces, connects, or removes a processor.
/// `POST /api/runtime/shutdown` is the one route that acts on the node rather
/// than reporting on it, and it sits behind the bearer-token auth middleware
/// when `auth_token` is `Some` (auth opted in); with `None` — the
/// zero-ceremony default — it is open like every other route. The GET routes,
/// health check, WebSocket event stream, and OpenAPI spec are always open.
/// `route_layer` binds the auth layer to exactly the routes already on the
/// protected sub-router, so a later `merge` leaves the open routes ungated.
pub(crate) fn build_router(
    runtime: Arc<dyn RuntimeOperations>,
    auth_token: Option<ApiServerBearerToken>,
) -> Router {
    // The read-only tap WebSocket is gated exactly like the shutdown route WHEN
    // auth is opted in — same bearer middleware, same route_layer binding; the
    // default (auth off) leaves it open like every other route. This is
    // mechanism parity, not a trust boundary the tap itself imposes. Clone the
    // token before it is moved into the protected-route middleware below.
    let tap_auth_token = auth_token.clone();
    // The MCP endpoint fronts the same shutdown op as a tool, so it is gated
    // the same way when auth is opted in.
    let mcp_auth_token = auth_token.clone();

    let mut protected = bearer_gated_routes();
    if let Some(auth_token) = auth_token {
        protected = protected.route_layer(axum::middleware::from_fn_with_state(
            auth_token,
            crate::auth::require_bearer_token,
        ));
    }

    let (router, openapi) = OpenApiRouter::with_openapi(ApiDoc::openapi())
        .merge(always_open_routes())
        .merge(protected)
        .split_for_parts();

    let state = AppState {
        runtime,
        openapi,
    };

    // TraceLayer logs all HTTP requests with method, path, status, and latency.
    let trace_layer = TraceLayer::new_for_http()
        .make_span_with(DefaultMakeSpan::new().level(Level::INFO))
        .on_request(DefaultOnRequest::new().level(Level::INFO))
        .on_response(DefaultOnResponse::new().level(Level::INFO));

    let mut tap_router = Router::new().route("/ws/tap/{channel}", get(tap_websocket_handler));
    if let Some(tap_auth_token) = tap_auth_token {
        tap_router = tap_router.route_layer(axum::middleware::from_fn_with_state(
            tap_auth_token,
            crate::auth::require_bearer_token,
        ));
    }

    let mut mcp_router = Router::new().route("/mcp", post(crate::mcp::mcp_endpoint));
    if let Some(mcp_auth_token) = mcp_auth_token {
        mcp_router = mcp_router.route_layer(axum::middleware::from_fn_with_state(
            mcp_auth_token,
            crate::auth::require_bearer_token,
        ));
    }

    let router = router
        .route("/ws/events", get(websocket_handler))
        .route("/api/openapi.json", get(get_openapi_spec))
        .merge(tap_router)
        .merge(mcp_router);

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

#[utoipa::path(
    get,
    path = "/api/graph",
    tag = "graph",
    responses(
        (status = 200, description = "Current graph state as JSON"),
        (status = 500, description = "Internal server error")
    )
)]
pub(crate) async fn get_graph(
    State(state): State<AppState>,
) -> std::result::Result<Json<serde_json::Value>, axum::http::StatusCode> {
    state
        .runtime
        .to_json_async()
        .await
        .map(Json)
        .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)
}

#[utoipa::path(
    post,
    path = "/api/runtime/shutdown",
    tag = "runtime",
    request_body = RuntimeShutdownRequest,
    responses(
        (status = 202, description = "Shutdown request accepted and handed to the runtime; teardown proceeds asynchronously and is NOT awaited by this response", body = RuntimeShutdownAcceptedResponse),
        (status = 401, description = "Missing or malformed bearer token", body = UnauthorizedResponse),
        (status = 403, description = "Invalid bearer token", body = ForbiddenResponse),
        (status = 500, description = "The request could not be handed to the runtime", body = ErrorResponse)
    )
)]
pub(crate) async fn request_runtime_shutdown(
    State(state): State<AppState>,
    Json(body): Json<RuntimeShutdownRequest>,
) -> axum::response::Response {
    let reason = body.reason.unwrap_or_default();

    // Never await teardown: this control plane is torn down BY the shutdown it
    // just requested (`ApiServerProcessor::stop` fires the graceful-shutdown
    // signal for this very server), so a handler that waited would be racing
    // its own socket. Hand the request to the runtime and answer 202.
    match state.runtime.request_runtime_shutdown(&reason) {
        Ok(()) => (
            StatusCode::ACCEPTED,
            Json(RuntimeShutdownAcceptedResponse {
                status: crate::state::RUNTIME_SHUTDOWN_REQUESTED_STATUS,
                reason,
            }),
        )
            .into_response(),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: error.to_string(),
            }),
        )
            .into_response(),
    }
}

/// Header carrying the width the surface's own backing holds, so a caller
/// that asked for a downscaled image still learns the true resolution —
/// the PNG's own header states only what was encoded.
///
/// Parsed once rather than per response: `HeaderName::from_static` panics on
/// a malformed name, and once at first use beats once per 200.
static SURFACE_PIXEL_WIDTH_HEADER: std::sync::LazyLock<axum::http::HeaderName> =
    std::sync::LazyLock::new(|| {
        axum::http::HeaderName::from_static("x-streamlib-surface-pixel-width")
    });

/// Height counterpart of [`SURFACE_PIXEL_WIDTH_HEADER`].
static SURFACE_PIXEL_HEIGHT_HEADER: std::sync::LazyLock<axum::http::HeaderName> =
    std::sync::LazyLock::new(|| {
        axum::http::HeaderName::from_static("x-streamlib-surface-pixel-height")
    });

/// The REST spelling of the exchange, as an OpenAPI path template.
///
/// The MCP tool names this route in its result so a caller that needs the
/// exact bytes has one call to make; a test asserts the served spec
/// carries exactly this path, so the two spellings cannot drift.
pub(crate) const SURFACE_IMAGE_EXCHANGE_ROUTE_PATH_TEMPLATE: &str =
    "/api/surfaces/{surface_id}/image";

/// RFC 3986's unreserved set: everything outside it is percent-encoded into
/// the route's `{surface_id}` segment. A pooled frame id is
/// `<slot>#<generation>`, and a bare `#` would make the generation a URL
/// fragment.
const SURFACE_ID_PATH_SEGMENT_PERCENT_ENCODE_ASCII_SET: &percent_encoding::AsciiSet =
    &percent_encoding::NON_ALPHANUMERIC
        .remove(b'-')
        .remove(b'.')
        .remove(b'_')
        .remove(b'~');

/// This route's path for one surface id, ready to put on the wire.
pub(crate) fn surface_image_exchange_route_path_for_surface_id(
    published_surface_id: &str,
) -> String {
    let path_segment = percent_encoding::utf8_percent_encode(
        published_surface_id,
        SURFACE_ID_PATH_SEGMENT_PERCENT_ENCODE_ASCII_SET,
    )
    .to_string();
    SURFACE_IMAGE_EXCHANGE_ROUTE_PATH_TEMPLATE.replace("{surface_id}", &path_segment)
}

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
        (status = 401, description = "Missing or malformed bearer token", body = UnauthorizedResponse),
        (status = 403, description = "Invalid bearer token", body = ForbiddenResponse),
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
        .runtime
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

/// Status for a refused exchange.
///
/// A recycled frame is its own answer: the id was well-formed and the
/// frame is gone, so the caller taps a newer bag rather than concluding the
/// surface never existed.
fn surface_exchange_failure_response(failure: &Error) -> Response {
    let status = match failure {
        Error::SurfaceFrameRecycled { .. } => StatusCode::GONE,
        Error::NotFound(_) => StatusCode::NOT_FOUND,
        Error::NotSupported(_) => StatusCode::NOT_IMPLEMENTED,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    };
    (
        status,
        Json(ErrorResponse {
            error: failure.to_string(),
        }),
    )
        .into_response()
}

#[utoipa::path(
    get,
    path = "/api/registry",
    tag = "registry",
    responses(
        (status = 200, description = "Available processor types", body = RegistryResponse)
    )
)]
pub(crate) async fn get_registry() -> Json<RegistryResponse> {
    let processors: Vec<ProcessorDescriptorOutput> = PROCESSOR_REGISTRY
        .list_registered()
        .into_iter()
        .map(|d| ProcessorDescriptorOutput::from(&d))
        .collect();

    Json(RegistryResponse { processors })
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

    // Listener that forwards events to channel
    let listener = Arc::new(Mutex::new(WebSocketEventForwarder { tx }));

    // Subscribe to ALL topics via wildcard. `subscribe` blocks until its
    // iceoryx2 subscriber is registered, so it must not run on an async worker.
    let listener_for_subscription: Arc<Mutex<dyn EventListener>> = listener.clone();
    match tokio::task::spawn_blocking(move || {
        PUBSUB.subscribe(topics::ALL, listener_for_subscription)
    })
    .await
    {
        Ok(Ok(())) => {}
        // Closing beats serving a client that would receive nothing forever.
        Ok(Err(subscribe_error)) => {
            tracing::warn!("WebSocket client not subscribed, closing: {subscribe_error}");
            return;
        }
        Err(join_error) => {
            tracing::warn!("event subscribe task failed to join: {join_error}");
            return;
        }
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

    // Cleanup
    drop(listener); // Weak ref cleanup on next publish
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

/// Query parameters for the tap WebSocket: an optional bounded sample count.
#[derive(Deserialize)]
pub(crate) struct TapQuery {
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
        ("channel" = String, Path, description = "Name of the channel to observe"),
        ("count" = Option<usize>, Query, description = "Stream exactly this many bags then close; absent streams live until the client disconnects")
    ),
    responses(
        (status = 101, description = "WebSocket upgraded. Read-only observability tap: each channel bag is forwarded verbatim (FrameHeader-framed) as a binary WS frame with no encode, containerize, or transcode — decoding is the client's concern. To observe a viewable video feed, tap an encoded (h264/h265/jpeg) or container (CMAF/fMP4) channel; a raw video channel carries zero-copy DMA-BUF/VkImage frame descriptors (meaningless off-host), not pixels, and this is not a realtime-video transport (use the WebRTC/MoQ/display processors)."),
        (status = 401, description = "Missing or malformed bearer token", body = UnauthorizedResponse),
        (status = 403, description = "Invalid bearer token", body = ForbiddenResponse)
    )
)]
pub(crate) async fn tap_websocket_handler(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    Path(channel): Path<String>,
    Query(query): Query<TapQuery>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_tap_websocket(socket, state.runtime, channel, query.count))
}

async fn handle_tap_websocket(
    socket: WebSocket,
    runtime: Arc<dyn RuntimeOperations>,
    channel: String,
    count: Option<usize>,
) {
    let (mut sender, mut receiver) = socket.split();

    // Attach the tap; a resolution / slot-occupied failure closes the socket
    // with the typed reason rather than silently hanging.
    let mut subscription = match runtime.tap_async(channel.clone(), count).await {
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
mod router_surface_and_auth_gate_tests {
    //! What [`build_router`] exposes, and how the bearer gate binds to it.
    //!
    //! Two things are under test. First, the route *surface*: the control plane
    //! is observation-shaped, so the router must expose no route that mutates
    //! the graph — a node's graph comes from its code. Second, the auth gate:
    //! `POST /api/runtime/shutdown` is the one route that acts on the node, and
    //! it carries the bearer middleware when auth is opted in.
    //!
    //! The router is the real one; only the `RuntimeOperations` backend is a
    //! stub.

    use super::*;
    use crate::control_plane_stub_support::{
        STUB_EXCHANGED_FRAME_SURFACE_ID, STUB_EXCHANGED_FRAME_SURFACE_ID_PERCENT_ENCODED,
        STUB_EXCHANGED_IMAGE_BYTES, STUB_SOURCE_SURFACE_EXTENT, StubSurfaceExchange,
    };
    use axum::body::Body;
    use axum::http::{
        Request, StatusCode,
        header::{AUTHORIZATION, CONTENT_TYPE},
    };
    use streamlib::sdk::runtime::BoxFuture;
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
    #[derive(Default)]
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
        fn request_runtime_shutdown(&self, reason: &str) -> Result<()> {
            self.recorded_shutdown_reasons
                .lock()
                .push(reason.to_string());
            Ok(())
        }
        fn tap_async(
            &self,
            channel: String,
            _count: Option<usize>,
        ) -> BoxFuture<'_, Result<streamlib::sdk::runtime::TapSubscription>> {
            Box::pin(async move { Err(Error::TapChannelNotFound(channel)) })
        }

        crate::control_plane_stub_support::surface_exchange_op_answers_the_stub!();
        crate::control_plane_stub_support::graph_mutation_ops_are_unreachable!("route");
    }

    const TEST_TOKEN: &str = "test-bearer-secret";

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

    fn auth_enabled_router() -> Router {
        build_router(
            Arc::new(ControlPlaneRouterStubRuntime::default()),
            Some(ApiServerBearerToken::from_secret(TEST_TOKEN)),
        )
    }

    /// Router in the default (auth-off) mode — every route is open with no token.
    fn auth_disabled_router() -> Router {
        build_router(
            Arc::new(ControlPlaneRouterStubRuntime::default()),
            None,
        )
    }

    async fn status_on(router: Router, request: Request<Body>) -> StatusCode {
        router.oneshot(request).await.unwrap().status()
    }

    async fn status_of(request: Request<Body>) -> StatusCode {
        status_on(auth_enabled_router(), request).await
    }

    fn runtime_shutdown_body() -> Body {
        Body::from(serde_json::json!({ "reason": "operator asked" }).to_string())
    }

    fn bearer(token: &str) -> String {
        format!("Bearer {token}")
    }

    async fn json_body_on(router: Router, request: Request<Body>) -> serde_json::Value {
        let response = router.oneshot(request).await.unwrap();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    /// The load-bearing surface assertion: no graph-mutation route is served.
    /// Checked with auth OFF so a 401 from the bearer gate can never be
    /// mistaken for the route's absence — with the gate out of the way, only a
    /// genuinely unrouted path answers 404/405.
    #[tokio::test]
    async fn the_router_serves_no_graph_mutation_route() {
        for (method, uri) in DELETED_GRAPH_MUTATION_ROUTES {
            let request = Request::builder()
                .method(*method)
                .uri(*uri)
                .header(CONTENT_TYPE, "application/json")
                .body(Body::from("{}"))
                .unwrap();
            let status = status_on(auth_disabled_router(), request).await;
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
        let spec = json_body_on(auth_enabled_router(), request).await;
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
        let served = json_body_on(auth_enabled_router(), request).await;
        let generated = serde_json::to_value(control_plane_openapi_spec())
            .expect("the generated spec serializes");
        assert_eq!(
            served, generated,
            "`generate_openapi` and the served `/api/openapi.json` must not diverge"
        );
    }

    #[tokio::test]
    async fn open_routes_need_no_authorization_header() {
        let open = ["/health", "/api/registry", "/api/openapi.json"];
        for uri in open {
            let request = Request::builder()
                .method("GET")
                .uri(uri)
                .body(Body::empty())
                .unwrap();
            assert_eq!(
                status_of(request).await,
                StatusCode::OK,
                "GET {uri} must stay open (no bearer token)"
            );
        }
    }

    #[tokio::test]
    async fn graph_is_open_and_reaches_the_runtime() {
        let request = Request::builder()
            .method("GET")
            .uri("/api/graph")
            .body(Body::empty())
            .unwrap();
        assert_eq!(status_of(request).await, StatusCode::OK);
    }

    #[tokio::test]
    async fn runtime_shutdown_rejects_a_missing_token_with_401() {
        let request = Request::builder()
            .method("POST")
            .uri("/api/runtime/shutdown")
            .header(CONTENT_TYPE, "application/json")
            .body(runtime_shutdown_body())
            .unwrap();
        assert_eq!(status_of(request).await, StatusCode::UNAUTHORIZED);
    }

    /// The shutdown request must reach the runtime handle — a 202 alone would
    /// also be produced by a handler that dropped the request on the floor —
    /// and it must answer 202 (accepted), never 200, because teardown is not
    /// awaited.
    #[tokio::test]
    async fn runtime_shutdown_with_token_is_202_and_reaches_the_runtime() {
        let runtime = Arc::new(ControlPlaneRouterStubRuntime::default());
        let recorded = runtime.recorded_shutdown_reasons.clone();
        let router = build_router(
            runtime,
            Some(ApiServerBearerToken::from_secret(TEST_TOKEN)),
        );
        let request = Request::builder()
            .method("POST")
            .uri("/api/runtime/shutdown")
            .header(AUTHORIZATION, bearer(TEST_TOKEN))
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

    /// A wrong token is a 403 (present but invalid), distinct from the 401 a
    /// missing token earns.
    #[tokio::test]
    async fn runtime_shutdown_with_a_wrong_token_is_403() {
        let request = Request::builder()
            .method("POST")
            .uri("/api/runtime/shutdown")
            .header(AUTHORIZATION, bearer("not-the-secret"))
            .header(CONTENT_TYPE, "application/json")
            .body(runtime_shutdown_body())
            .unwrap();
        assert_eq!(status_of(request).await, StatusCode::FORBIDDEN);
    }

    /// The zero-ceremony default: auth off leaves the shutdown route open.
    #[tokio::test]
    async fn runtime_shutdown_is_open_with_auth_off() {
        let request = Request::builder()
            .method("POST")
            .uri("/api/runtime/shutdown")
            .header(CONTENT_TYPE, "application/json")
            .body(runtime_shutdown_body())
            .unwrap();
        assert_eq!(
            status_on(auth_disabled_router(), request).await,
            StatusCode::ACCEPTED,
            "POST /api/runtime/shutdown must be open with auth off (no token)"
        );
    }

    /// An omitted `reason` is unspecified, not a 400 — the request is the
    /// point, the attribution is a courtesy.
    #[tokio::test]
    async fn runtime_shutdown_without_a_reason_is_accepted_as_unspecified() {
        let runtime = Arc::new(ControlPlaneRouterStubRuntime::default());
        let recorded = runtime.recorded_shutdown_reasons.clone();
        let router = build_router(
            runtime,
            None,
        );
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

    fn tap_ws_request() -> Request<Body> {
        // A plain GET (no upgrade headers): enough to exercise the bearer gate,
        // which runs as a `route_layer` BEFORE the WS upgrade extractor.
        Request::builder()
            .method("GET")
            .uri("/ws/tap/some-channel")
            .body(Body::empty())
            .unwrap()
    }

    #[tokio::test]
    async fn tap_ws_rejects_missing_token_with_401_when_auth_on() {
        // With auth opted in, the read-only tap is gated exactly like the
        // shutdown route — mechanism parity, not a trust boundary the tap
        // imposes. Deleting the tap_router `.route_layer(...)` flips this from
        // 401 to the WS extractor's own (non-401) rejection, going red here.
        assert_eq!(status_of(tap_ws_request()).await, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn tap_ws_with_token_clears_the_auth_gate() {
        // A valid token passes the gate; the request then reaches the WS handler,
        // whose upgrade extractor rejects this non-upgrade GET with a non-401
        // status — proving the gate admitted it rather than rejecting it.
        let mut request = tap_ws_request();
        request
            .headers_mut()
            .insert(AUTHORIZATION, bearer(TEST_TOKEN).try_into().unwrap());
        assert_ne!(status_of(request).await, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn tap_ws_is_open_with_auth_off() {
        assert_ne!(
            status_on(auth_disabled_router(), tap_ws_request()).await,
            StatusCode::UNAUTHORIZED,
            "GET /ws/tap/{{channel}} must be reachable with auth off (no token)"
        );
    }

    // ------------------------------------------------------------------
    // Surface exchange: a published surface id in, image bytes out
    // ------------------------------------------------------------------

    fn router_with_bearer_auth(runtime: ControlPlaneRouterStubRuntime) -> Router {
        build_router(
            Arc::new(runtime),
            Some(ApiServerBearerToken::from_secret(TEST_TOKEN)),
        )
    }

    fn router_without_bearer_auth(runtime: ControlPlaneRouterStubRuntime) -> Router {
        build_router(
            Arc::new(runtime),
            None,
        )
    }

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
        let response = router_without_bearer_auth(runtime)
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
        let status = router_without_bearer_auth(runtime)
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
        let response = router_without_bearer_auth(runtime)
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

    /// The exchange joins the bearer-gated set beside the tap WebSocket —
    /// same middleware, same binding. Deleting it from `bearer_gated_routes`
    /// flips this from 401 to 200.
    #[tokio::test]
    async fn the_exchange_route_rejects_a_missing_token_with_401_when_auth_on() {
        assert_eq!(
            status_on(
                router_with_bearer_auth(ControlPlaneRouterStubRuntime::default()),
                exchange_request(&exchange_uri(
                    STUB_EXCHANGED_FRAME_SURFACE_ID_PERCENT_ENCODED
                ))
            )
            .await,
            StatusCode::UNAUTHORIZED
        );
    }

    #[tokio::test]
    async fn the_exchange_route_rejects_a_wrong_token_with_403() {
        let mut request = exchange_request(&exchange_uri(
            STUB_EXCHANGED_FRAME_SURFACE_ID_PERCENT_ENCODED,
        ));
        request
            .headers_mut()
            .insert(AUTHORIZATION, bearer("not-the-secret").try_into().unwrap());
        assert_eq!(
            status_on(
                router_with_bearer_auth(ControlPlaneRouterStubRuntime::default()),
                request
            )
            .await,
            StatusCode::FORBIDDEN
        );
    }

    #[tokio::test]
    async fn the_exchange_route_serves_the_image_with_a_valid_token() {
        let mut request = exchange_request(&exchange_uri(
            STUB_EXCHANGED_FRAME_SURFACE_ID_PERCENT_ENCODED,
        ));
        request
            .headers_mut()
            .insert(AUTHORIZATION, bearer(TEST_TOKEN).try_into().unwrap());
        assert_eq!(
            status_on(
                router_with_bearer_auth(ControlPlaneRouterStubRuntime::default()),
                request
            )
            .await,
            StatusCode::OK
        );
    }

    /// The zero-ceremony default: with auth off the exchange is open like
    /// every other observation route.
    #[tokio::test]
    async fn the_exchange_route_is_open_with_auth_off() {
        assert_eq!(
            status_on(
                auth_disabled_router(),
                exchange_request(&exchange_uri(
                    STUB_EXCHANGED_FRAME_SURFACE_ID_PERCENT_ENCODED
                ))
            )
            .await,
            StatusCode::OK
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
