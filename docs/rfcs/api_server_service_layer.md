# RFC: API Server Processor

## Status: Draft

## Summary

Build an `ApiServerProcessor` - a ManualProcessor that runs an Axum HTTP/WebSocket server, exposing StreamRuntime control via REST API. This enables building web UIs for StreamLib.

## Motivation

StreamLib needs a way for external applications (web UIs, control panels, automation tools) to:
- Create/remove processors dynamically
- Connect/disconnect ports
- Start/stop/pause/resume the runtime
- Subscribe to real-time events (processor added, frame processed, errors)

The runtime already provides `RuntimeOperations` with async variants. We need an HTTP layer to expose these operations externally.

## Current Infrastructure

The following already exists and will be used:

### Shared Tokio Runtime
```rust
// StreamRuntime owns a tokio runtime
pub struct StreamRuntime {
    pub(crate) tokio_runtime: tokio::runtime::Runtime,
    // ...
}

// Processors access it via RuntimeContext
impl RuntimeContext {
    pub fn tokio_handle(&self) -> &tokio::runtime::Handle { ... }
}
```

### RuntimeOperations Trait
```rust
pub trait RuntimeOperations: Send + Sync {
    // Async variants - safe from tokio tasks
    fn add_processor_async(&self, spec: ProcessorSpec) -> BoxFuture<'_, Result<ProcessorUniqueId>>;
    fn remove_processor_async(&self, processor_id: ProcessorUniqueId) -> BoxFuture<'_, Result<()>>;
    fn connect_async(&self, from: OutputLinkPortRef, to: InputLinkPortRef) -> BoxFuture<'_, Result<LinkUniqueId>>;
    fn disconnect_async(&self, link_id: LinkUniqueId) -> BoxFuture<'_, Result<()>>;

    // Sync wrappers (NOT safe from tokio tasks)
    fn add_processor(&self, spec: ProcessorSpec) -> Result<ProcessorUniqueId>;
    // ...
}
```

### ManualProcessor Lifecycle
```rust
pub trait ManualProcessor {
    fn setup(&mut self, ctx: RuntimeContext) -> impl Future<Output = Result<()>> + Send;
    fn start(&mut self) -> Result<()>;
    fn stop(&mut self) -> Result<()>;
    fn teardown(&mut self) -> impl Future<Output = Result<()>> + Send;
}
```

### PUBSUB Event System
```rust
// Processors can subscribe to runtime events
PUBSUB.subscribe(topics::RUNTIME_GLOBAL, listener);

// Events include:
RuntimeEvent::ProcessorAdded { processor_id }
RuntimeEvent::ProcessorRemoved { processor_id }
RuntimeEvent::LinkCreated { link_id }
RuntimeEvent::GraphDidChange
// etc.
```

## Design

### Architecture

```
┌─────────────────────────────────────────────────────────────────┐
│  StreamRuntime                                                  │
│                                                                 │
│  ┌──────────────────────────────────────────────────────────┐  │
│  │  ApiServerProcessor (ManualProcessor)                     │  │
│  │                                                           │  │
│  │  setup(): Store RuntimeContext, subscribe to PUBSUB       │  │
│  │  start(): Spawn Axum server task on ctx.tokio_handle()    │  │
│  │  stop(): Signal shutdown, wait for server to stop         │  │
│  │  teardown(): Cleanup                                      │  │
│  │                                                           │  │
│  │  ┌─────────────────────────────────────────────────────┐ │  │
│  │  │  Axum Server (runs on shared tokio runtime)         │ │  │
│  │  │  - REST endpoints call ctx.runtime().*_async()      │ │  │
│  │  │  - WebSocket broadcasts PUBSUB events to clients    │ │  │
│  │  └─────────────────────────────────────────────────────┘ │  │
│  └──────────────────────────────────────────────────────────┘  │
│                                                                 │
│  ┌──────────────────────────────────────────────────────────┐  │
│  │  Other Processors (Camera, Display, etc.)                 │  │
│  └──────────────────────────────────────────────────────────┘  │
└─────────────────────────────────────────────────────────────────┘
```

### Key Design Decisions

1. **Use shared tokio runtime** - No separate thread. Spawn server task via `ctx.tokio_handle().spawn()`.

2. **Async RuntimeOperations** - HTTP handlers call `ctx.runtime().add_processor_async().await` directly. No channel-based proxy needed.

3. **PUBSUB for events** - Subscribe to `RUNTIME_GLOBAL` topic, broadcast to WebSocket clients.

4. **Graceful shutdown** - Use `tokio::sync::oneshot` to signal server shutdown from `stop()`.

## Implementation

### Processor Definition

```rust
#[streamlib::processor(
    execution = Manual,
    name = "ApiServerProcessor",
    description = "HTTP/WebSocket API for runtime control"
)]
pub struct ApiServerProcessor {
    #[streamlib::config]
    config: ApiServerConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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
```

### Processor State

```rust
struct ApiServerState {
    /// Runtime operations for graph mutations
    runtime_ops: Arc<dyn RuntimeOperations>,
    /// Broadcast channel for WebSocket clients
    event_tx: broadcast::Sender<RuntimeEvent>,
}

impl ApiServerProcessor::Processor {
    // Internal state (not in macro-generated struct)
    runtime_ctx: Option<RuntimeContext>,
    shutdown_tx: Option<oneshot::Sender<()>>,
    server_handle: Option<tokio::task::JoinHandle<()>>,
}
```

### Lifecycle Implementation

```rust
impl ManualProcessor for ApiServerProcessor::Processor {
    async fn setup(&mut self, ctx: RuntimeContext) -> Result<()> {
        self.runtime_ctx = Some(ctx);
        Ok(())
    }

    fn start(&mut self) -> Result<()> {
        let ctx = self.runtime_ctx.as_ref()
            .ok_or_else(|| StreamError::Runtime("setup() not called".into()))?;

        let config = self.config.clone();
        let runtime_ops = ctx.runtime();
        let tokio_handle = ctx.tokio_handle().clone();

        // Create shutdown channel
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        self.shutdown_tx = Some(shutdown_tx);

        // Create event broadcast channel
        let (event_tx, _) = broadcast::channel(256);

        // Subscribe to PUBSUB and forward to broadcast channel
        let event_tx_clone = event_tx.clone();
        Self::subscribe_to_pubsub(event_tx_clone);

        // Build shared state for handlers
        let state = Arc::new(ApiServerState {
            runtime_ops,
            event_tx,
        });

        // Spawn server on shared tokio runtime
        let handle = tokio_handle.spawn(async move {
            run_server(config, state, shutdown_rx).await;
        });
        self.server_handle = Some(handle);

        tracing::info!("API server starting on {}:{}", config.host, config.port);
        Ok(())
    }

    fn stop(&mut self) -> Result<()> {
        // Signal shutdown
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }

        // Wait for server to stop (with timeout)
        if let Some(handle) = self.server_handle.take() {
            // Use block_on since stop() is sync
            let ctx = self.runtime_ctx.as_ref().unwrap();
            let _ = ctx.tokio_handle().block_on(async {
                tokio::time::timeout(
                    std::time::Duration::from_secs(5),
                    handle
                ).await
            });
        }

        tracing::info!("API server stopped");
        Ok(())
    }

    async fn teardown(&mut self) -> Result<()> {
        self.runtime_ctx = None;
        Ok(())
    }
}
```

### HTTP Server

```rust
async fn run_server(
    config: ApiServerConfig,
    state: Arc<ApiServerState>,
    shutdown_rx: oneshot::Receiver<()>,
) {
    let app = Router::new()
        // Health check
        .route("/health", get(|| async { "ok" }))
        // Processor endpoints
        .route("/api/processors", post(create_processor).get(list_processors))
        .route("/api/processors/:id", get(get_processor).delete(remove_processor))
        // Connection endpoints
        .route("/api/connections", post(create_connection).get(list_connections))
        .route("/api/connections/:id", delete(remove_connection))
        // Lifecycle endpoints
        .route("/api/runtime/state", get(get_runtime_state))
        // WebSocket for events
        .route("/api/events", get(websocket_handler))
        // CORS for browser clients
        .layer(CorsLayer::permissive())
        .with_state(state);

    let addr = format!("{}:{}", config.host, config.port);
    let listener = TcpListener::bind(&addr).await.unwrap();

    tracing::info!("API server listening on {}", addr);

    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            let _ = shutdown_rx.await;
            tracing::info!("API server shutting down");
        })
        .await
        .unwrap();
}
```

### REST Handlers

```rust
#[derive(Deserialize)]
struct CreateProcessorRequest {
    processor_type: String,
    config: serde_json::Value,
}

#[derive(Serialize)]
struct CreateProcessorResponse {
    processor_id: String,
}

async fn create_processor(
    State(state): State<Arc<ApiServerState>>,
    Json(request): Json<CreateProcessorRequest>,
) -> Result<Json<CreateProcessorResponse>, AppError> {
    let spec = ProcessorSpec::new(&request.processor_type, request.config);

    // Call async variant - safe from tokio task
    let processor_id = state.runtime_ops.add_processor_async(spec).await?;

    Ok(Json(CreateProcessorResponse {
        processor_id: processor_id.to_string(),
    }))
}

async fn remove_processor(
    State(state): State<Arc<ApiServerState>>,
    Path(id): Path<String>,
) -> Result<StatusCode, AppError> {
    let processor_id = ProcessorUniqueId::from(id);
    state.runtime_ops.remove_processor_async(processor_id).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct CreateConnectionRequest {
    from_processor: String,
    from_port: String,
    to_processor: String,
    to_port: String,
}

#[derive(Serialize)]
struct CreateConnectionResponse {
    link_id: String,
}

async fn create_connection(
    State(state): State<Arc<ApiServerState>>,
    Json(request): Json<CreateConnectionRequest>,
) -> Result<Json<CreateConnectionResponse>, AppError> {
    let from = OutputLinkPortRef::new(
        &ProcessorUniqueId::from(request.from_processor),
        &request.from_port,
    );
    let to = InputLinkPortRef::new(
        &ProcessorUniqueId::from(request.to_processor),
        &request.to_port,
    );

    let link_id = state.runtime_ops.connect_async(from, to).await?;

    Ok(Json(CreateConnectionResponse {
        link_id: link_id.to_string(),
    }))
}
```

### Error Handling

```rust
struct AppError(StreamError);

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let status = match &self.0 {
            StreamError::ProcessorNotFound(_) => StatusCode::NOT_FOUND,
            StreamError::LinkNotFound(_) => StatusCode::NOT_FOUND,
            StreamError::Config(_) => StatusCode::BAD_REQUEST,
            StreamError::InvalidPort(_) => StatusCode::BAD_REQUEST,
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        };

        let body = Json(serde_json::json!({
            "error": self.0.to_string()
        }));

        (status, body).into_response()
    }
}

impl From<StreamError> for AppError {
    fn from(err: StreamError) -> Self {
        AppError(err)
    }
}
```

### WebSocket Event Streaming

```rust
async fn websocket_handler(
    ws: WebSocketUpgrade,
    State(state): State<Arc<ApiServerState>>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_websocket(socket, state))
}

async fn handle_websocket(mut socket: WebSocket, state: Arc<ApiServerState>) {
    let mut event_rx = state.event_tx.subscribe();

    loop {
        tokio::select! {
            // Forward runtime events to WebSocket client
            result = event_rx.recv() => {
                match result {
                    Ok(event) => {
                        let json = serde_json::to_string(&event).unwrap();
                        if socket.send(Message::Text(json)).await.is_err() {
                            break; // Client disconnected
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!("WebSocket client lagged, dropped {} events", n);
                        continue;
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        break;
                    }
                }
            }

            // Handle incoming WebSocket messages
            msg = socket.recv() => {
                match msg {
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Ok(Message::Ping(data))) => {
                        let _ = socket.send(Message::Pong(data)).await;
                    }
                    _ => {} // Ignore other messages for now
                }
            }
        }
    }
}
```

### PUBSUB Bridge

```rust
impl ApiServerProcessor::Processor {
    fn subscribe_to_pubsub(event_tx: broadcast::Sender<RuntimeEvent>) {
        struct EventForwarder {
            tx: broadcast::Sender<RuntimeEvent>,
        }

        impl EventListener for EventForwarder {
            fn on_event(&mut self, event: &Event) -> Result<()> {
                if let Event::RuntimeGlobal(runtime_event) = event {
                    // Ignore send errors (no subscribers yet is OK)
                    let _ = self.tx.send(runtime_event.clone());
                }
                Ok(())
            }
        }

        let forwarder = EventForwarder { tx: event_tx };
        PUBSUB.subscribe(
            topics::RUNTIME_GLOBAL,
            Arc::new(Mutex::new(forwarder)),
        );
    }
}
```

## API Reference

### Processors

| Method | Endpoint | Description |
|--------|----------|-------------|
| POST | `/api/processors` | Create processor |
| GET | `/api/processors` | List all processors |
| GET | `/api/processors/:id` | Get processor details |
| DELETE | `/api/processors/:id` | Remove processor |

### Connections

| Method | Endpoint | Description |
|--------|----------|-------------|
| POST | `/api/connections` | Create connection |
| GET | `/api/connections` | List all connections |
| DELETE | `/api/connections/:id` | Remove connection |

### Runtime

| Method | Endpoint | Description |
|--------|----------|-------------|
| GET | `/api/runtime/state` | Get runtime state (JSON graph export) |
| GET | `/health` | Health check |

### WebSocket

| Endpoint | Description |
|----------|-------------|
| `/api/events` | Subscribe to runtime events |

## Request/Response Examples

### Create Processor

```http
POST /api/processors
Content-Type: application/json

{
  "processor_type": "CameraProcessor",
  "config": {
    "device_id": null
  }
}
```

```json
{
  "processor_id": "camera-abc123"
}
```

### Create Connection

```http
POST /api/connections
Content-Type: application/json

{
  "from_processor": "camera-abc123",
  "from_port": "video",
  "to_processor": "display-def456",
  "to_port": "video"
}
```

```json
{
  "link_id": "link-xyz789"
}
```

### WebSocket Events

```json
{"type": "ProcessorAdded", "processor_id": "camera-abc123"}
{"type": "LinkCreated", "link_id": "link-xyz789"}
{"type": "GraphDidChange"}
```

## File Structure

```
libs/streamlib/src/core/processors/
├── api_server/
│   ├── mod.rs           # Processor definition, ManualProcessor impl
│   ├── server.rs        # Axum server setup, run_server()
│   ├── handlers.rs      # REST endpoint handlers
│   ├── websocket.rs     # WebSocket handler, PUBSUB bridge
│   └── error.rs         # AppError, response mapping
```

## Dependencies

```toml
[dependencies]
# Already in workspace
tokio = { version = "1", features = ["rt-multi-thread", "net", "sync", "time"] }

# New dependencies
axum = { version = "0.8", features = ["ws"] }
tower-http = { version = "0.6", features = ["cors"] }
```

## Feature Flag

The API server should be behind a feature flag to avoid pulling in HTTP dependencies for embedded use cases:

```toml
[features]
default = []
api-server = ["axum", "tower-http"]
```

## Open Questions

1. **Authentication**: Should we add API key or JWT auth for production use?
2. **Rate limiting**: Protect against API abuse?
3. **Pause/Resume**: Expose `runtime.pause()` / `runtime.resume()` via REST?
4. **Config updates**: Add `PATCH /api/processors/:id/config` for runtime config changes?
5. **Metrics endpoint**: Add `/api/metrics` for Prometheus scraping?

## Implementation Checklist

- [ ] Add `axum` and `tower-http` to Cargo.toml (feature-gated)
- [ ] Create `api_server/mod.rs` with processor definition
- [ ] Implement `ManualProcessor` lifecycle
- [ ] Create `api_server/server.rs` with Axum setup
- [ ] Create `api_server/handlers.rs` with REST endpoints
- [ ] Create `api_server/websocket.rs` with event streaming
- [ ] Create `api_server/error.rs` with error handling
- [ ] Add PUBSUB bridge for event forwarding
- [ ] Add integration tests
- [ ] Create example: `examples/api-server-demo/`
