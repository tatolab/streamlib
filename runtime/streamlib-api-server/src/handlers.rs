// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! HTTP + WebSocket handlers, wired into the router by [`build_router`].

use axum::{
    Json, Router,
    extract::Path,
    extract::Query,
    extract::State,
    extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
};
use futures_util::{SinkExt, StreamExt};
use parking_lot::Mutex;
use serde::Deserialize;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
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
    OpenApiRouter::new().routes(routes!(request_runtime_shutdown))
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
    #[cfg(feature = "moq")] runtime_id: String,
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
        #[cfg(feature = "moq")]
        runtime_id,
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

    #[cfg(feature = "moq")]
    let router = router.route("/api/moq/catalog", get(get_moq_catalog));

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

/// MoQ broadcast catalog with currently-published tracks.
///
/// Returns an empty catalog when no MoQ publish processor has touched this
/// runtime yet — the package-global session registry in `@tatolab/moq` is
/// populated lazily on first publish.
#[cfg(feature = "moq")]
pub(crate) async fn get_moq_catalog(
    State(state): State<AppState>,
) -> Json<streamlib_moq::MoqBroadcastCatalog> {
    let mut catalog = streamlib_moq::MoqBroadcastCatalog::new();
    if let Some(sessions) = streamlib_moq::try_sessions_for_runtime(&state.runtime_id) {
        for track_name in sessions.published_track_names() {
            catalog.add_track(&track_name, None, &track_name);
        }
    }
    Json(catalog)
}

// ============================================================================
// WebSocket subscription-live contract
// ============================================================================

/// Build a close frame, truncating the reason to what RFC 6455 permits.
///
/// Every close on these sockets goes through here: the cap is a wire invariant
/// (tungstenite refuses an over-length control frame and the client gets an
/// abnormal close with no reason at all), and a per-site `truncate` call is an
/// invariant held by memory.
fn websocket_close_frame(code: u16, reason: impl Into<String>) -> Message {
    Message::Close(Some(CloseFrame {
        code,
        reason: truncate_on_char_boundary(reason.into(), MAX_WS_CLOSE_REASON_BYTES).into(),
    }))
}

/// Close code for a socket that could not be opened — the graph snapshot its
/// first frame carries could not be produced. App codes live in the 4000–4999
/// private range, alongside the tap's 4404 / 4409.
const WS_CLOSE_CODE_STREAM_UNAVAILABLE: u16 = 4503;

/// Close code for a client that fell too far behind its event stream. Distinct
/// from every other close so a client knows the cure is to reconnect and read
/// the fresh snapshot, not to retry blindly.
const WS_CLOSE_CODE_CLIENT_LAGGED: u16 = 4504;

/// Events a client may fall behind by before it is closed as lagged.
///
/// Bounded rather than unbounded because `publish` runs on the engine's threads:
/// an unbounded queue makes a slow socket the node's memory problem. Dropping the
/// client is acceptable only because reconnecting is lossless — it opens with a
/// fresh snapshot — which is the whole reason the stream leads with state.
const MAX_BUFFERED_EVENTS_PER_CLIENT: usize = 1024;

/// The non-`Event` frame a control-plane WebSocket opens with. It precedes every
/// data frame on the socket and says the subscription behind it is attached.
///
/// `/ws/events` is a best-effort realtime stream, not a record: it carries no
/// history and replays nothing. Current state is `GET /api/graph` and the
/// durable record is the JSONL log — a client that wants either asks for it,
/// which is what this frame makes safe to do. Waiting for it before reading the
/// graph composes without a gap, because the subscription is attached before the
/// read, so anything the read misses arrives as a following event.
///
/// Wire contract: `Event` is an externally tagged enum, so an event frame is
/// always a single-key JSON object keyed by a variant name (`RuntimeGlobal`,
/// `ProcessorEvent`, `Custom`). These variants keep that grammar with keys that
/// are none of them, so a client discriminates on the key alone and a strict
/// `Event` decoder rejects one as an unknown variant rather than mis-reading it.
/// On `/ws/tap/{channel}` the separation is stronger still — this is the only
/// text frame that socket carries, and every bag stays a verbatim binary frame.
#[derive(serde::Serialize)]
enum ControlPlaneWebSocketOpeningFrame {
    EventStreamSubscriptionLive { topic: String },
    TapSubscriptionLive { channel: String },
}

impl ControlPlaneWebSocketOpeningFrame {
    /// Render as the text frame to put on the wire.
    fn to_websocket_text_frame(&self) -> Result<Message> {
        serde_json::to_string(self)
            .map(|json| Message::Text(json.into()))
            .map_err(|e| Error::Runtime(format!("opening frame could not be serialized: {e}")))
    }
}

// ============================================================================
// WebSocket Event Streaming
// ============================================================================

/// `GET /ws/events` — stream the node's runtime events, opening with the graph.
#[utoipa::path(
    get,
    path = "/ws/events",
    tag = "events",
    responses(
        (status = 101, description = "WebSocket upgraded. Best-effort realtime event stream: it carries no history and replays nothing. The first frame is text — {\"EventStreamSubscriptionLive\":{\"topic\":\"*\"}} — sent once the subscription is attached; every frame after it is one runtime Event as JSON. Wait for that frame before reading GET /api/graph if you want current state, and the two compose without a gap: the subscription is attached before the read, so anything the read misses arrives as a following event. Current state is /api/graph and the durable record is the JSONL log — neither is served here. Frames are discriminated by their single top-level key: the opening frame's key is never an Event variant name (RuntimeGlobal / ProcessorEvent / Custom), so a strict Event decoder rejects it rather than mis-reading it. A client that falls more than 1024 events behind is closed with 4504 and should reconnect.")
    )
)]
pub(crate) async fn websocket_handler(ws: WebSocketUpgrade) -> impl IntoResponse {
    ws.on_upgrade(handle_websocket)
}

async fn handle_websocket(socket: WebSocket) {
    let (mut sender, mut receiver) = socket.split();

    // Channel to bridge sync EventListener -> async WebSocket
    let (tx, mut rx) = tokio::sync::mpsc::channel::<Event>(MAX_BUFFERED_EVENTS_PER_CLIENT);
    let client_lagged = Arc::new(AtomicBool::new(false));

    // Listener that forwards events to channel
    let listener = Arc::new(Mutex::new(WebSocketEventForwarder {
        tx: Some(tx),
        client_lagged: Arc::clone(&client_lagged),
    }));

    // Subscribe to ALL topics via wildcard. Registration is synchronous, so
    // every event caused from here on is queued for this socket — including any
    // caused while the snapshot below is being taken.
    PUBSUB.subscribe(topics::ALL, listener.clone());

    // The subscription is attached above, so this frame reports a fact rather
    // than a promise. axum runs this callback after the 101, so the upgrade
    // alone cannot tell a client that — which is the whole reason a first frame
    // exists on a stream that otherwise just streams.
    let opening_frame = ControlPlaneWebSocketOpeningFrame::EventStreamSubscriptionLive {
        topic: topics::ALL.to_string(),
    }
    .to_websocket_text_frame();

    let opening_frame = match opening_frame {
        Ok(opening_frame) => opening_frame,
        Err(e) => {
            tracing::error!("WebSocket opening frame could not be built: {e}");
            let _ = sender
                .send(websocket_close_frame(
                    WS_CLOSE_CODE_STREAM_UNAVAILABLE,
                    format!("event stream could not start: {e}"),
                ))
                .await;
            return;
        }
    };

    if sender.send(opening_frame).await.is_err() {
        return;
    }

    tracing::info!("WebSocket client connected, subscription to all events is live");

    // Task: forward channel events to WebSocket
    let send_task = tokio::spawn(async move {
        while let Some(event) = rx.recv().await {
            match serde_json::to_string(&event) {
                Ok(json) => {
                    if sender.send(Message::Text(json.into())).await.is_err() {
                        return;
                    }
                }
                Err(e) => {
                    tracing::warn!("Failed to serialize event: {}", e);
                }
            }
        }

        // The loop ends only when every sender is gone, and the forwarder drops
        // its sender exactly when it latches the lag — so the flag is read after
        // the fact rather than raced against the drain.
        if client_lagged.load(Ordering::Relaxed) {
            tracing::info!("WebSocket client fell behind its event stream, closing");
            let _ = sender
                .send(websocket_close_frame(
                    WS_CLOSE_CODE_CLIENT_LAGGED,
                    "client lagged; reconnect for a fresh snapshot",
                ))
                .await;
        }
    });

    // Keep-alive / close, raced against the send task: that task ends when it
    // has closed the socket itself, and a client closed for lagging is exactly
    // the one that may never answer the close handshake — waiting only on the
    // client would keep this subscription alive cloning events for a corpse.
    let mut send_task = send_task;
    loop {
        tokio::select! {
            _ = &mut send_task => break,
            message = receiver.next() => match message {
                Some(Ok(Message::Close(_))) | None => {
                    tracing::info!("WebSocket client closed connection");
                    break;
                }
                Some(Err(e)) => {
                    tracing::warn!("WebSocket error: {}", e);
                    break;
                }
                // axum handles ping/pong automatically
                Some(Ok(_)) => {}
            },
        }
    }

    // Cleanup
    drop(listener); // Weak ref cleanup on next publish
    send_task.abort();
    tracing::info!("WebSocket client disconnected");
}

struct WebSocketEventForwarder {
    /// Taken when the client is found lagging, which closes the channel and
    /// ends the send task's drain — the signal the task acts on.
    tx: Option<tokio::sync::mpsc::Sender<Event>>,
    client_lagged: Arc<AtomicBool>,
}

impl EventListener for WebSocketEventForwarder {
    /// Hands the event to the socket's send task without blocking: `on_event`
    /// runs on the engine thread that published, so it must never wait on a
    /// client. A full queue latches the lag rather than dropping the event
    /// quietly — the socket then closes and the client re-snapshots.
    fn on_event(&mut self, event: &Event) -> Result<()> {
        // Reserve before cloning: a lagging client would otherwise pay a full
        // Event clone per publish only to have it dropped. The borrow on `tx`
        // ends with the permit, so the sender can be taken below.
        let queue_is_full = match self.tx.as_ref() {
            Some(tx) => match tx.try_reserve() {
                Ok(permit) => {
                    permit.send(event.clone());
                    false
                }
                Err(_) => true,
            },
            None => return Ok(()),
        };

        if queue_is_full {
            self.client_lagged.store(true, Ordering::Relaxed);
            self.tx = None;
        }
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
        (status = 101, description = "WebSocket upgraded. The first frame is text — {\"TapSubscriptionLive\":{\"channel\":\"…\"}} — sent once the tap is attached, so a client can act without racing the attach; it is the only text frame the socket carries. Every frame after it is a channel bag forwarded verbatim (FrameHeader-framed) as a binary WS frame with no encode, containerize, or transcode — decoding is the client's concern. To observe a viewable video feed, tap an encoded (h264/h265/jpeg) or container (CMAF/fMP4) channel; a raw video channel carries zero-copy DMA-BUF/VkImage frame descriptors (meaningless off-host), not pixels, and this is not a realtime-video transport (use the WebRTC/MoQ/display processors)."),
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

    // `tap_async` resolves only once the tap's subscriber exists, so this frame
    // reports an attach that has already happened — a client that acts on it
    // cannot race the attach, and can tell "attached" from "attached but idle".
    let live_frame_was_sent = match (ControlPlaneWebSocketOpeningFrame::TapSubscriptionLive {
        channel: channel.clone(),
    })
    .to_websocket_text_frame()
    {
        Ok(live_frame) => sender.send(live_frame).await.is_ok(),
        Err(e) => {
            tracing::error!(channel = %channel, "tap live frame could not be sent: {e}");
            false
        }
    };

    // Forwarding is skipped by falling through rather than returning: the
    // detach below must stay on the path out, because dropping the subscription
    // here would join an OS thread on an async worker.
    if live_frame_was_sent {
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

/// Longest close reason RFC 6455 permits: a control frame caps its payload
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
            #[cfg(feature = "moq")]
            "test-runtime-id".to_string(),
        )
    }

    /// Router in the default (auth-off) mode — every route is open with no token.
    fn auth_disabled_router() -> Router {
        build_router(
            Arc::new(ControlPlaneRouterStubRuntime::default()),
            None,
            #[cfg(feature = "moq")]
            "test-runtime-id".to_string(),
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
            #[cfg(feature = "moq")]
            "test-runtime-id".to_string(),
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
            #[cfg(feature = "moq")]
            "test-runtime-id".to_string(),
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
}

#[cfg(test)]
mod websocket_subscription_live_frame_tests {
    //! The subscription-live contract on the control plane's WebSockets.
    //!
    //! axum runs the upgrade callback after the 101, so the upgrade alone tells
    //! a client nothing about whether anything is listening on its behalf yet.
    //! These cover what the socket says instead: an opening frame that is
    //! unambiguously not an `Event`, arriving before any event does, and
    //! carrying the state a client would otherwise have to infer from deltas.

    use super::*;
    use crate::control_plane_stub_support::observation_ops_answer_an_empty_graph;
    use std::time::Duration;
    use streamlib::sdk::pubsub::RuntimeEvent;
    use streamlib::sdk::runtime::{BoxFuture, TapSubscription};

    /// The `Event` variant names an event frame can be keyed by. An opening
    /// frame keyed by any of these would be ambiguous with a real event.
    const EVENT_VARIANT_KEYS: &[&str] = &["RuntimeGlobal", "ProcessorEvent", "Custom"];

    /// Hands out one preset `TapSubscription`, so `/ws/tap/{channel}` can be
    /// driven over a real socket with no engine behind it.
    ///
    /// Separate from [`EventStreamStubRuntime`] for its `tap_async` alone; the
    /// observation ops both answer come from one macro so a new
    /// `RuntimeOperations` method is one edit rather than several diverging
    /// ones.
    struct TapStubRuntime {
        subscription: Mutex<Option<TapSubscription>>,
    }

    impl RuntimeOperations for TapStubRuntime {
        observation_ops_answer_an_empty_graph!("the tap test");

        fn tap_async(
            &self,
            channel: String,
            _count: Option<usize>,
        ) -> BoxFuture<'_, Result<TapSubscription>> {
            let taken = self.subscription.lock().take();
            Box::pin(async move { taken.ok_or(Error::TapSlotOccupied(channel)) })
        }

        crate::control_plane_stub_support::graph_mutation_ops_are_unreachable!("route");
    }

    /// Serve the real router on a loopback ephemeral port, returning the port
    /// and the task to abort when done.
    ///
    /// A real bind rather than `tower::oneshot`, which drives a router but never
    /// completes an upgrade — so it can never observe a frame.
    fn serve_on_ephemeral_port(
        runtime: Arc<dyn RuntimeOperations>,
    ) -> (u16, tokio::task::JoinHandle<()>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
        listener
            .set_nonblocking(true)
            .expect("the tokio listener needs a non-blocking socket");
        let port = listener.local_addr().expect("local addr").port();
        let listener =
            tokio::net::TcpListener::from_std(listener).expect("adopt the bound listener");

        let router = build_router(
            runtime,
            None,
            #[cfg(feature = "moq")]
            "test-runtime-id".to_string(),
        );
        let server = tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });
        (port, server)
    }

    /// How long a test waits for a frame the server should already be sending.
    /// Every read is bounded by it: a hung suite reports nothing, a red one
    /// names the frame that never came.
    const FRAME_ARRIVAL_BUDGET: Duration = Duration::from_secs(5);

    /// `/ws/events` reads nothing off the runtime — it subscribes to `PUBSUB`
    /// directly — but `build_router` needs one.
    struct EventStreamStubRuntime;

    impl RuntimeOperations for EventStreamStubRuntime {
        observation_ops_answer_an_empty_graph!("the event-stream test");

        fn tap_async(
            &self,
            channel: String,
            _count: Option<usize>,
        ) -> BoxFuture<'_, Result<TapSubscription>> {
            Box::pin(async move { Err(Error::TapChannelNotFound(channel)) })
        }

        crate::control_plane_stub_support::graph_mutation_ops_are_unreachable!("route");
    }

    fn opening_frame_json(frame: &ControlPlaneWebSocketOpeningFrame) -> serde_json::Value {
        serde_json::to_value(frame).expect("opening frame serializes")
    }

    /// The wire contract that lets a client tell the live frame from an event
    /// without out-of-band knowledge: both are single-key objects, and the live
    /// frame's key is none of `Event`'s.
    #[test]
    fn an_opening_frame_is_never_mistakable_for_an_event() {
        let frames = [
            ControlPlaneWebSocketOpeningFrame::EventStreamSubscriptionLive {
                topic: topics::ALL.to_string(),
            },
            ControlPlaneWebSocketOpeningFrame::TapSubscriptionLive {
                channel: "some-processor/some-output".to_string(),
            },
        ];

        for frame in &frames {
            let json = opening_frame_json(frame);
            let object = json.as_object().expect("opening frame is a JSON object");
            assert_eq!(
                object.len(),
                1,
                "an opening frame keeps Event's single-key grammar: {json}"
            );

            let key = object.keys().next().expect("single key");
            assert!(
                !EVENT_VARIANT_KEYS.contains(&key.as_str()),
                "opening frame key '{key}' collides with an Event variant"
            );

            // The other direction: a strict `Event` decoder must reject it
            // outright rather than mis-read it as some event.
            assert!(
                serde_json::from_value::<Event>(json.clone()).is_err(),
                "an opening frame must not deserialize as an Event: {json}"
            );
        }
    }

    /// Every event frame stays decodable as an `Event` — the opening frame is
    /// prepended, and no envelope is wrapped around the events themselves.
    #[test]
    fn an_event_frame_is_unchanged_by_the_opening_frame() {
        let event = Event::RuntimeGlobal(RuntimeEvent::GraphDidChange);
        let encoded = serde_json::to_string(&event).expect("event serializes");

        let decoded: Event = serde_json::from_str(&encoded).expect("event frame decodes as Event");
        assert_eq!(decoded, event);
    }

    /// The guarantee a client acts on: once the opening frame arrives, nothing
    /// published after it is missed.
    ///
    /// The publish below is not racing anything — `subscribe` registers
    /// synchronously inside the handler, before the frame goes out — so this
    /// locks the wire contract, and the engine's `core::pubsub::integration_tests`
    /// lock the delivery guarantee it rests on.
    ///
    /// The publish below is not racing anything — `subscribe` registers
    /// synchronously inside the handler — so this locks the wire contract, and
    /// the engine's `core::pubsub::integration_tests` lock the delivery
    /// guarantee it rests on.
    ///
    /// `#[serial]`: this test publishes, and `PUBSUB` is process-global — an
    /// unserialized publish here lands inside the sample window of any other
    /// test reading the same bus.
    #[tokio::test]
    #[serial_test::serial]
    async fn ws_events_sends_the_subscription_live_frame_before_any_event() {
        use tokio_tungstenite::tungstenite::Message as ClientMessage;

        let (port, server) = serve_on_ephemeral_port(Arc::new(EventStreamStubRuntime));

        let (mut socket, _) =
            tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}/ws/events"))
                .await
                .expect("WebSocket upgrade on /ws/events");

        // Frame 1 says the subscription is attached, before anything is published.
        // Bounded like every other read here: a regression that stops the frame
        // being sent must name itself, not hang the suite until the job's cap.
        let first = tokio::time::timeout(FRAME_ARRIVAL_BUDGET, socket.next())
            .await
            .expect("a first frame within the budget")
            .expect("a first frame")
            .expect("first frame is not an error");
        let ClientMessage::Text(first) = first else {
            panic!("the first frame must be text, got {first:?}");
        };
        let first: serde_json::Value = serde_json::from_str(&first).expect("first frame is JSON");
        assert!(
            first.get("EventStreamSubscriptionLive").is_some(),
            "first frame must be the subscription-live frame, got {first}"
        );

        let published = Event::RuntimeGlobal(RuntimeEvent::GraphDidChange);
        PUBSUB.publish(&published.topic(), &published);

        let next = tokio::time::timeout(FRAME_ARRIVAL_BUDGET, socket.next())
            .await
            .expect("an event frame within the budget")
            .expect("a second frame")
            .expect("second frame is not an error");
        let ClientMessage::Text(next) = next else {
            panic!("an event frame must be text, got {next:?}");
        };
        let received: Event = serde_json::from_str(&next)
            .expect("the frame after the live frame decodes as an Event");
        assert_eq!(received, published);

        let _ = socket.close(None).await;
        server.abort();
    }

    /// The tap socket's half of the same contract: a text live frame, then the
    /// bag — verbatim and binary.
    ///
    /// The OpenAPI 101 description promises exactly this shape, and nothing
    /// else proves the server sends it: the frame-grammar test only serializes
    /// the enum, and the MCP tap tool never touches this route.
    #[tokio::test]
    async fn ws_tap_sends_the_live_frame_before_any_bag() {
        use tokio_tungstenite::tungstenite::Message as ClientMessage;

        const BAG: &[u8] = b"\x00\x01\x02 not-an-event";

        let (bag_sender, bag_receiver) = tokio::sync::mpsc::channel(1);
        bag_sender.send(BAG.to_vec()).await.expect("queue one bag");

        let (port, server) = serve_on_ephemeral_port(Arc::new(TapStubRuntime {
            subscription: Mutex::new(Some(TapSubscription::from_forward_channel(
                "some-processor/some-output".to_string(),
                bag_receiver,
                0,
            ))),
        }));

        let (mut socket, _) = tokio_tungstenite::connect_async(format!(
            "ws://127.0.0.1:{port}/ws/tap/some-processor%2Fsome-output"
        ))
        .await
        .expect("WebSocket upgrade on /ws/tap/{channel}");

        let first = tokio::time::timeout(FRAME_ARRIVAL_BUDGET, socket.next())
            .await
            .expect("a first frame within the budget")
            .expect("a first frame")
            .expect("first frame is not an error");
        let ClientMessage::Text(first) = first else {
            panic!("the tap's first frame must be text, got {first:?}");
        };
        let first: serde_json::Value = serde_json::from_str(&first).expect("first frame is JSON");
        assert_eq!(
            first
                .get("TapSubscriptionLive")
                .and_then(|live| live.get("channel"))
                .and_then(serde_json::Value::as_str),
            Some("some-processor/some-output"),
            "first frame must be the tap's subscription-live frame, got {first}"
        );

        // The bag is binary and byte-identical: the live frame is prepended to
        // the stream, never wrapped around what the channel carries.
        let next = tokio::time::timeout(FRAME_ARRIVAL_BUDGET, socket.next())
            .await
            .expect("a bag frame within the budget")
            .expect("a second frame")
            .expect("second frame is not an error");
        let ClientMessage::Binary(bag) = next else {
            panic!("a bag must arrive as a binary frame, got {next:?}");
        };
        assert_eq!(bag.as_ref(), BAG, "the bag must be forwarded verbatim");

        let _ = socket.close(None).await;
        server.abort();
    }
}
