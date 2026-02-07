// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use crate::core::pubsub::{topics, Event, EventListener, PUBSUB};
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
use std::path::PathBuf;
use std::sync::Arc;
use streamlib_broker::proto::broker_service_client::BrokerServiceClient;
use streamlib_broker::proto::{RegisterRuntimeRequest, UnregisterRuntimeRequest};
use streamlib_broker::GRPC_PORT;
use tower_http::trace::{DefaultMakeSpan, DefaultOnRequest, DefaultOnResponse, TraceLayer};
use tracing::Level;
use utoipa::OpenApi;
use utoipa_axum::{router::OpenApiRouter, routes};

/// Docker-style adjectives for runtime name generation.
const ADJECTIVES: &[&str] = &[
    "admiring",
    "brave",
    "clever",
    "dazzling",
    "eager",
    "fancy",
    "graceful",
    "happy",
    "inspiring",
    "jolly",
    "keen",
    "lively",
    "merry",
    "noble",
    "optimistic",
    "peaceful",
    "quirky",
    "radiant",
    "serene",
    "trusting",
    "upbeat",
    "vibrant",
    "witty",
    "xenial",
    "youthful",
    "zealous",
];

/// Docker-style nouns for runtime name generation.
const NOUNS: &[&str] = &[
    "albatross",
    "beaver",
    "cheetah",
    "dolphin",
    "eagle",
    "falcon",
    "gazelle",
    "hawk",
    "ibis",
    "jaguar",
    "koala",
    "leopard",
    "meerkat",
    "nightingale",
    "otter",
    "panther",
    "quail",
    "raven",
    "sparrow",
    "tiger",
    "urchin",
    "viper",
    "walrus",
    "xerus",
    "yak",
    "zebra",
];

/// Generate a Docker-style random name (adjective-noun).
fn generate_runtime_name() -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    use std::time::{SystemTime, UNIX_EPOCH};

    // Use time + pid for randomness without adding fastrand dependency
    let seed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos() as u64
        ^ (std::process::id() as u64);
    let mut hasher = DefaultHasher::new();
    seed.hash(&mut hasher);
    let hash = hasher.finish();

    let adj = ADJECTIVES[(hash as usize) % ADJECTIVES.len()];
    let noun = NOUNS[((hash >> 32) as usize) % NOUNS.len()];
    format!("{}-{}", adj, noun)
}

/// Get the broker gRPC endpoint from environment or default.
fn broker_endpoint() -> String {
    let port = std::env::var("STREAMLIB_BROKER_PORT")
        .ok()
        .and_then(|p| p.parse::<u16>().ok())
        .unwrap_or(GRPC_PORT);
    format!("http://127.0.0.1:{}", port)
}

/// Get the default logs directory (~/.streamlib/logs).
fn default_logs_dir() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".streamlib").join("logs"))
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
    ProcessorDescriptorOutput, RegistryResponse, SchemaDescriptorOutput, SemanticVersionOutput,
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
        (name = "schemas", description = "Schema definitions"),
        (name = "events", description = "Real-time event streaming via WebSocket")
    )
)]
struct ApiDoc;

// ============================================================================
// Processor Definition
// ============================================================================

#[crate::processor("src/core/processors/api_server.yaml")]
pub struct ApiServerProcessor {
    runtime_ctx: Option<RuntimeContext>,
    shutdown_tx: Option<tokio::sync::oneshot::Sender<()>>,
    runtime_id: Option<String>,
    resolved_name: Option<String>,
    actual_port: Option<u16>,
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

        // Resolve runtime name (from config or auto-generate)
        let runtime_name = self
            .config
            .name
            .clone()
            .unwrap_or_else(generate_runtime_name);
        self.resolved_name = Some(runtime_name.clone());

        // Get runtime ID from env (set by CLI) or generate one
        let runtime_id = std::env::var("STREAMLIB_RUNTIME_ID")
            .unwrap_or_else(|_| format!("R{}", cuid2::create_id()));
        self.runtime_id = Some(runtime_id.clone());

        // Resolve log path (from config or derive from name)
        let log_path: PathBuf = self
            .config
            .log_path
            .clone()
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                default_logs_dir()
                    .unwrap_or_else(|| PathBuf::from("/tmp/streamlib/logs"))
                    .join(format!("{}.log", runtime_name))
            });

        // Build OpenAPI router with documented routes
        let (router, openapi) = OpenApiRouter::with_openapi(ApiDoc::openapi())
            .routes(routes!(health))
            .routes(routes!(get_graph))
            .routes(routes!(create_processor))
            .routes(routes!(delete_processor))
            .routes(routes!(create_connection))
            .routes(routes!(delete_connection))
            .routes(routes!(get_registry))
            .routes(routes!(list_schema_definitions))
            .routes(routes!(get_schema_definition))
            .split_for_parts();

        let state = AppState {
            runtime_ctx: ctx.clone(),
            openapi,
        };

        // Add WebSocket route and OpenAPI spec endpoint (not documented in OpenAPI)
        // TraceLayer logs all HTTP requests with method, path, status, and latency
        let trace_layer = TraceLayer::new_for_http()
            .make_span_with(DefaultMakeSpan::new().level(Level::INFO))
            .on_request(DefaultOnRequest::new().level(Level::INFO))
            .on_response(DefaultOnResponse::new().level(Level::INFO));

        let app = router
            .route("/ws/events", get(websocket_handler))
            .route("/api/openapi.json", get(get_openapi_spec))
            .layer(trace_layer)
            .with_state(state);

        let config = self.config.clone();
        let host = config.host.clone();
        let base_port = config.port;

        // Try to bind to port, incrementing if in use (up to 10 attempts)
        let (listener, actual_port) = ctx.tokio_handle().block_on(async {
            for port_offset in 0..10u16 {
                let port = base_port + port_offset;
                let addr = format!("{}:{}", host, port);
                match tokio::net::TcpListener::bind(&addr).await {
                    Ok(listener) => {
                        if port_offset > 0 {
                            tracing::info!("Port {} in use, bound to {} instead", base_port, port);
                        }
                        return Ok((listener, port));
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
                        continue;
                    }
                    Err(e) => {
                        return Err(crate::core::StreamError::Other(anyhow::anyhow!(
                            "Failed to bind to {}: {}",
                            addr,
                            e
                        )));
                    }
                }
            }
            Err(crate::core::StreamError::Other(anyhow::anyhow!(
                "Could not find available port in range {}-{}",
                base_port,
                base_port + 9
            )))
        })?;

        self.actual_port = Some(actual_port);
        let api_endpoint = format!("{}:{}", host, actual_port);

        tracing::info!("Api server listening on {}", api_endpoint);
        tracing::info!(
            "OpenAPI spec available at http://{}/api/openapi.json",
            api_endpoint
        );

        // Register with broker (non-blocking, continues even if broker unavailable)
        let log_path_str = log_path.to_string_lossy().to_string();
        let pid = std::process::id() as i32;
        let endpoint = broker_endpoint();
        let reg_runtime_id = runtime_id.clone();
        let reg_name = runtime_name.clone();
        let reg_api_endpoint = api_endpoint.clone();

        ctx.tokio_handle().spawn(async move {
            match BrokerServiceClient::connect(endpoint.clone()).await {
                Ok(mut client) => {
                    match client
                        .register_runtime(RegisterRuntimeRequest {
                            runtime_id: reg_runtime_id,
                            name: reg_name,
                            api_endpoint: reg_api_endpoint,
                            log_path: log_path_str,
                            pid,
                        })
                        .await
                    {
                        Ok(_) => {
                            tracing::info!("Registered with broker at {}", endpoint);
                        }
                        Err(e) => {
                            tracing::warn!("Failed to register with broker: {}", e);
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        "Could not connect to broker at {}: {}. Runtime will run standalone.",
                        endpoint,
                        e
                    );
                }
            }
        });

        // Spawn the HTTP server
        ctx.tokio_handle().spawn(async move {
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
        // Unregister from broker before stopping
        if let Some(runtime_id) = self.runtime_id.take() {
            let endpoint = broker_endpoint();
            // Use a new tokio runtime for cleanup since we may not have access to the original
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build();
            if let Ok(rt) = rt {
                rt.block_on(async {
                    if let Ok(mut client) = BrokerServiceClient::connect(endpoint).await {
                        let _ = client
                            .unregister_runtime(UnregisterRuntimeRequest {
                                runtime_id: runtime_id.clone(),
                            })
                            .await;
                        tracing::info!("Unregistered runtime {} from broker", runtime_id);
                    }
                });
            }
        }

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

    let schemas: Vec<SchemaDescriptorOutput> = PROCESSOR_REGISTRY
        .known_schemas()
        .into_iter()
        .map(|name| SchemaDescriptorOutput {
            name,
            version: SemanticVersionOutput {
                major: 1,
                minor: 0,
                patch: 0,
            },
            fields: vec![],
            read_behavior: Default::default(),
            default_capacity: 0,
        })
        .collect();

    Json(RegistryResponse {
        processors,
        schemas,
    })
}

#[utoipa::path(
    get,
    path = "/api/schemas",
    tag = "schemas",
    responses(
        (status = 200, description = "List of schema names that have embedded definitions", body = Vec<String>)
    )
)]
async fn list_schema_definitions() -> Json<Vec<String>> {
    Json(
        crate::core::embedded_schemas::list_embedded_schema_names()
            .into_iter()
            .map(|s| s.to_string())
            .collect(),
    )
}

#[utoipa::path(
    get,
    path = "/api/schemas/{name}",
    tag = "schemas",
    params(
        ("name" = String, Path, description = "Schema name (e.g. com.tatolab.videoframe)")
    ),
    responses(
        (status = 200, description = "YAML schema definition", body = String),
        (status = 404, description = "Schema not found")
    )
)]
async fn get_schema_definition(
    Path(name): Path<String>,
) -> std::result::Result<String, axum::http::StatusCode> {
    crate::core::embedded_schemas::get_embedded_schema_definition(&name)
        .map(|def| def.to_string())
        .ok_or(axum::http::StatusCode::NOT_FOUND)
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
