// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use crate::core::pubsub::{topics, Event, EventListener, PUBSUB};
use crate::core::schema_registry::SCHEMA_REGISTRY;
use crate::core::{InputLinkPortRef, OutputLinkPortRef};
use crate::PROCESSOR_REGISTRY;
use crate::{
    core::{Result, RuntimeContext},
    ProcessorSpec,
};
use axum::{
    extract::ws::{Message, WebSocket, WebSocketUpgrade},
    extract::Path,
    extract::State,
    response::IntoResponse,
    Json,
};
use futures_util::{SinkExt, StreamExt};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::future::Future;
use std::sync::Arc;
use utoipa::OpenApi;
use utoipa_axum::{router::OpenApiRouter, routes};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, crate::ConfigDescriptor)]
pub struct ApiServerConfig {
    pub host: String,
    pub port: u16,
}

impl Default for ApiServerConfig {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".to_string(),
            port: 9000,
        }
    }
}

#[derive(Clone)]
struct AppState {
    runtime_ctx: RuntimeContext,
    openapi: utoipa::openapi::OpenApi,
}

// ============================================================================
// Request/Response Types with OpenAPI Schema
// ============================================================================

#[derive(Deserialize, utoipa::ToSchema)]
struct CreateProcessorRequest {
    /// The processor type name (e.g., "CameraProcessor", "DisplayProcessor")
    processor_type: String,
    /// Processor-specific configuration as JSON
    config: serde_json::Value,
}

#[derive(Deserialize, utoipa::ToSchema)]
struct CreateConnectionRequest {
    /// Source processor ID
    from_processor: String,
    /// Source output port name
    from_port: String,
    /// Destination processor ID
    to_processor: String,
    /// Destination input port name
    to_port: String,
}

#[derive(Serialize, utoipa::ToSchema)]
struct IdResponse {
    /// The created resource ID
    id: String,
}

// Note: RegistryResponse is now defined in crate::core::json_schema
// and imported below for the get_registry handler.
use crate::core::json_schema::{
    ProcessorDescriptorOutput, RegistryResponse, SchemaDescriptorOutput,
};

#[derive(Serialize, utoipa::ToSchema)]
struct ErrorResponse {
    /// Error message
    error: String,
}

// ============================================================================
// OpenAPI Documentation
// ============================================================================

#[derive(OpenApi)]
#[openapi(
    info(
        title = "StreamLib Runtime API",
        version = "0.1.0",
        description = "Real-time streaming infrastructure API for managing processors, connections, and events",
        license(name = "BUSL-1.1")
    ),
    tags(
        (name = "graph", description = "Graph inspection and management"),
        (name = "processors", description = "Processor lifecycle management"),
        (name = "connections", description = "Connection management between processors"),
        (name = "registry", description = "Processor and schema registry"),
        (name = "events", description = "Real-time event streaming via WebSocket")
    )
)]
struct ApiDoc;

// ============================================================================
// Processor Definition
// ============================================================================

#[crate::processor(
    execution = Manual,
    description = "Runtime api server for streamlib"
)]
pub struct ApiServerProcessor {
    #[crate::config]
    config: ApiServerConfig,
    runtime_ctx: Option<RuntimeContext>,
    shutdown_tx: Option<tokio::sync::oneshot::Sender<()>>,
}

impl crate::core::ManualProcessor for ApiServerProcessor::Processor {
    fn setup(&mut self, ctx: RuntimeContext) -> impl Future<Output = Result<()>> + Send {
        self.runtime_ctx = Some(ctx);
        std::future::ready(Ok(()))
    }

    fn teardown(&mut self) -> impl Future<Output = Result<()>> + Send {
        std::future::ready(Ok(()))
    }

    fn on_pause(&mut self) -> impl Future<Output = Result<()>> + Send {
        std::future::ready(Ok(()))
    }

    fn on_resume(&mut self) -> impl Future<Output = Result<()>> + Send {
        std::future::ready(Ok(()))
    }

    fn start(&mut self) -> Result<()> {
        use axum::routing::get;

        let ctx = self
            .runtime_ctx
            .clone()
            .expect("setup must be called before start");
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
        self.shutdown_tx = Some(shutdown_tx);

        // Build OpenAPI router with documented routes
        let (router, openapi) = OpenApiRouter::with_openapi(ApiDoc::openapi())
            .routes(routes!(health))
            .routes(routes!(get_graph))
            .routes(routes!(create_processor))
            .routes(routes!(delete_processor))
            .routes(routes!(create_connection))
            .routes(routes!(delete_connection))
            .routes(routes!(get_registry))
            .split_for_parts();

        let state = AppState {
            runtime_ctx: ctx.clone(),
            openapi,
        };

        // Add WebSocket route and OpenAPI spec endpoint (not documented in OpenAPI)
        let app = router
            .route("/ws/events", get(websocket_handler))
            .route("/api/openapi.json", get(get_openapi_spec))
            .with_state(state);

        let config = self.config.clone();
        let addr = format!("{}:{}", config.host, config.port);

        ctx.tokio_handle().spawn(async move {
            let listener = tokio::net::TcpListener::bind(&addr).await.unwrap();
            tracing::info!("Api server listening on {}", addr);
            tracing::info!("OpenAPI spec available at http://{}/api/openapi.json", addr);
            axum::serve(listener, app)
                .with_graceful_shutdown(async {
                    let _ = shutdown_rx.await;
                })
                .await
                .unwrap();
        });

        Ok(())
    }

    fn stop(&mut self) -> Result<()> {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
        Ok(())
    }
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
async fn health() -> &'static str {
    tracing::info!("Health function called");
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
async fn get_graph(
    State(state): State<AppState>,
) -> std::result::Result<Json<serde_json::Value>, axum::http::StatusCode> {
    state
        .runtime_ctx
        .runtime()
        .to_json_async()
        .await
        .map(Json)
        .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)
}

#[utoipa::path(
    post,
    path = "/api/processor",
    tag = "processors",
    request_body = CreateProcessorRequest,
    responses(
        (status = 200, description = "Processor created successfully", body = IdResponse),
        (status = 400, description = "Invalid processor type or configuration", body = ErrorResponse)
    )
)]
async fn create_processor(
    State(state): State<AppState>,
    Json(body): Json<CreateProcessorRequest>,
) -> std::result::Result<Json<IdResponse>, axum::http::StatusCode> {
    let spec = ProcessorSpec::new(&body.processor_type, body.config);

    state
        .runtime_ctx
        .runtime()
        .add_processor_async(spec)
        .await
        .map(|id| Json(IdResponse { id: id.to_string() }))
        .map_err(|_| axum::http::StatusCode::BAD_REQUEST)
}

#[utoipa::path(
    delete,
    path = "/api/processors/{id}",
    tag = "processors",
    params(
        ("id" = String, Path, description = "Processor ID to delete")
    ),
    responses(
        (status = 204, description = "Processor deleted successfully"),
        (status = 404, description = "Processor not found")
    )
)]
async fn delete_processor(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> std::result::Result<axum::http::StatusCode, axum::http::StatusCode> {
    let processor_id = id.into();
    state
        .runtime_ctx
        .runtime()
        .remove_processor_async(processor_id)
        .await
        .map(|_| axum::http::StatusCode::NO_CONTENT)
        .map_err(|_| axum::http::StatusCode::NOT_FOUND)
}

#[utoipa::path(
    post,
    path = "/api/connections",
    tag = "connections",
    request_body = CreateConnectionRequest,
    responses(
        (status = 200, description = "Connection created successfully", body = IdResponse),
        (status = 400, description = "Invalid connection (ports don't exist or types don't match)", body = ErrorResponse)
    )
)]
async fn create_connection(
    State(state): State<AppState>,
    Json(body): Json<CreateConnectionRequest>,
) -> std::result::Result<Json<IdResponse>, axum::http::StatusCode> {
    let from = OutputLinkPortRef::new(body.from_processor, body.from_port);
    let to = InputLinkPortRef::new(body.to_processor, body.to_port);

    state
        .runtime_ctx
        .runtime()
        .connect_async(from, to)
        .await
        .map(|id| Json(IdResponse { id: id.to_string() }))
        .map_err(|_| axum::http::StatusCode::BAD_REQUEST)
}

#[utoipa::path(
    delete,
    path = "/api/connections/{id}",
    tag = "connections",
    params(
        ("id" = String, Path, description = "Connection ID to delete")
    ),
    responses(
        (status = 204, description = "Connection deleted successfully"),
        (status = 404, description = "Connection not found")
    )
)]
async fn delete_connection(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> std::result::Result<axum::http::StatusCode, axum::http::StatusCode> {
    let link_id = id.into();

    state
        .runtime_ctx
        .runtime()
        .disconnect_async(link_id)
        .await
        .map(|_| axum::http::StatusCode::NO_CONTENT)
        .map_err(|_| axum::http::StatusCode::NOT_FOUND)
}

#[utoipa::path(
    get,
    path = "/api/registry",
    tag = "registry",
    responses(
        (status = 200, description = "Available processors and schemas", body = RegistryResponse)
    )
)]
async fn get_registry() -> Json<RegistryResponse> {
    let processors: Vec<ProcessorDescriptorOutput> = PROCESSOR_REGISTRY
        .list_registered()
        .into_iter()
        .map(|d| ProcessorDescriptorOutput::from(&d))
        .collect();
    let schemas: Vec<SchemaDescriptorOutput> = SCHEMA_REGISTRY
        .list_descriptors()
        .into_iter()
        .map(|d| SchemaDescriptorOutput::from(&d))
        .collect();
    Json(RegistryResponse {
        processors,
        schemas,
    })
}

async fn get_openapi_spec(State(state): State<AppState>) -> Json<utoipa::openapi::OpenApi> {
    Json(state.openapi)
}

// ============================================================================
// WebSocket Event Streaming
// ============================================================================

async fn websocket_handler(ws: WebSocketUpgrade) -> impl IntoResponse {
    ws.on_upgrade(handle_websocket)
}

async fn handle_websocket(socket: WebSocket) {
    let (mut sender, mut receiver) = socket.split();

    // Channel to bridge sync EventListener -> async WebSocket
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Event>();

    // Listener that forwards events to channel
    let listener = Arc::new(Mutex::new(WebSocketEventForwarder { tx }));

    // Subscribe to ALL topics via wildcard
    PUBSUB.subscribe(topics::ALL, listener.clone());

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
