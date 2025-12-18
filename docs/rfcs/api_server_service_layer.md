# RFC: API Server and Service Layer Architecture

## Status: Draft

## Summary

Thread-safe service layer for controlling StreamRuntime from external clients (HTTP/WebSocket). Treats the API server as a processor type, isolating Tokio to background threads while maintaining main thread control for platform hardware.

---

## Dependencies

This RFC depends on:
- [Dynamic Processor and Link Creation API](api_dynamic_creation.md) - ProcessorNodeFactory for string-based processor creation

---

## Threading Model

```
┌─────────────────────────────────────────────────────────────┐
│  MAIN THREAD (OS thread 0)                                  │
│  - StreamRuntime owns command channel                       │
│  - Polls commands in event loop                             │
│  - Executes: add_processor(), connect(), start(), etc.      │
│  - macOS: NSApplication event loop                          │
└─────────────────────────────────────────────────────────────┘
                           ▲
                           │ commands via flume channel
                           │ responses via oneshot
┌──────────────────────────┴──────────────────────────────────┐
│  TOKIO RUNTIME (background thread)                          │
│  - ApiServerProcessor runs HTTP/WS server                   │
│  - Uses RuntimeService to send commands                     │
│  - Broadcasts events to WebSocket clients                   │
└─────────────────────────────────────────────────────────────┘
```

### Why This Architecture

Apple frameworks (AVFoundation, VideoToolbox, CoreMedia) require main thread execution. Tokio must be isolated to background threads to avoid blocking hardware access.

### Key Insight: Not Duplicating Runtime

The service layer is **not** a duplicate of StreamRuntime. It's a thread-safe async facade:

1. Processor calls `service.add_processor(spec).await`
2. Service serializes this into a `RuntimeCommand::AddProcessor`
3. Command sent over channel to main thread
4. Main thread executes `runtime.add_processor(spec)` (the real implementation)
5. Result sent back via oneshot channel

The service contains **zero business logic** - it's purely a cross-thread communication mechanism.

---

## RuntimeService

Ergonomic async interface for controlling the runtime from any thread. Obtained via `RuntimeContext`.

```rust
/// Thread-safe async facade for StreamRuntime operations.
///
/// Obtained from `RuntimeContext::runtime_service()` in processor setup.
/// All methods are async and safe to call from Tokio threads.
#[derive(Clone)]
pub struct RuntimeService {
    command_tx: flume::Sender<(RuntimeCommand, oneshot::Sender<CommandResult>)>,
    event_tx: broadcast::Sender<RuntimeEvent>,
}

impl RuntimeService {
    /// Add a processor to the runtime.
    pub async fn add_processor(&self, spec: ProcessorSpec) -> Result<ProcessorUniqueId>;

    /// Remove a processor from the runtime.
    pub async fn remove_processor(&self, id: ProcessorUniqueId) -> Result<()>;

    /// Connect two ports.
    pub async fn connect(
        &self,
        from: OutputLinkPortRef,
        to: InputLinkPortRef
    ) -> Result<LinkUniqueId>;

    /// Disconnect a link.
    pub async fn disconnect(&self, link_id: LinkUniqueId) -> Result<()>;

    /// Start the runtime.
    pub async fn start(&self) -> Result<()>;

    /// Stop the runtime.
    pub async fn stop(&self) -> Result<()>;

    /// Get current runtime state.
    pub async fn get_state(&self) -> Result<RuntimeState>;

    /// Subscribe to runtime events (for WebSocket broadcasting).
    pub fn subscribe_events(&self) -> broadcast::Receiver<RuntimeEvent>;
}
```

### How Processors Obtain RuntimeService

```rust
impl crate::core::Processor for MyProcessor::Processor {
    fn setup(&mut self, ctx: &RuntimeContext) -> Result<()> {
        // RuntimeContext provides the service
        self.runtime_service = Some(ctx.runtime_service());
        Ok(())
    }
}
```

### Internal Command Types

```rust
/// Internal command enum - not exposed to users.
/// Users interact via RuntimeService's ergonomic methods.
pub(crate) enum RuntimeCommand {
    AddProcessor { spec: ProcessorSpec },
    RemoveProcessor { id: ProcessorUniqueId },
    Connect { from: OutputLinkPortRef, to: InputLinkPortRef },
    Disconnect { link_id: LinkUniqueId },
    Start,
    Stop,
    GetState,
}

pub(crate) enum CommandResult {
    ProcessorAdded(ProcessorUniqueId),
    ProcessorRemoved,
    Connected(LinkUniqueId),
    Disconnected,
    Started,
    Stopped,
    State(RuntimeState),
    Error(StreamError),
}
```

---

## StreamRuntime Integration

The runtime owns the command channel and polls it in its event loop.

```rust
impl StreamRuntime {
    /// Create runtime with service layer enabled.
    pub fn new() -> Result<Self> {
        let (command_tx, command_rx) = flume::unbounded();
        let (event_tx, _) = broadcast::channel(256);

        Self {
            // ... existing fields ...
            command_rx,
            runtime_service: RuntimeService { command_tx, event_tx },
        }
    }

    /// Poll and execute pending commands. Called from main thread event loop.
    pub fn poll_commands(&mut self) {
        while let Ok((command, response_tx)) = self.command_rx.try_recv() {
            let result = self.execute_command(command);
            let _ = response_tx.send(result);
        }
    }

    fn execute_command(&mut self, command: RuntimeCommand) -> CommandResult {
        match command {
            RuntimeCommand::AddProcessor { spec } => {
                match self.add_processor(spec) {
                    Ok(id) => CommandResult::ProcessorAdded(id),
                    Err(e) => CommandResult::Error(e),
                }
            }
            RuntimeCommand::Connect { from, to } => {
                match self.connect(from, to) {
                    Ok(link) => CommandResult::Connected(link.id()),
                    Err(e) => CommandResult::Error(e),
                }
            }
            // ... other commands delegate to existing runtime methods ...
        }
    }
}
```

### RuntimeContext Enhancement

```rust
impl RuntimeContext {
    /// Get a RuntimeService for cross-thread runtime control.
    ///
    /// Use this in processors that need to modify the graph dynamically
    /// (e.g., API servers, orchestration processors).
    pub fn runtime_service(&self) -> RuntimeService {
        self.runtime_service.clone()
    }
}
```

---

## ApiServerProcessor

API server as a processor type, fitting the streamlib pattern.

```rust
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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

#[streamlib::processor(
    execution = Manual,
    description = "HTTP/WebSocket API server for runtime control"
)]
pub struct ApiServerProcessor {
    #[streamlib::config]
    config: ApiServerConfig,

    // Obtained from RuntimeContext in setup()
    runtime_service: Option<RuntimeService>,

    // Handle to shutdown Tokio runtime
    shutdown_tx: Option<tokio::sync::oneshot::Sender<()>>,
}
```

### Lifecycle

1. **setup()**: Obtain RuntimeService from context, spawn Tokio runtime on background thread, start HTTP server
2. **process()**: No-op (server runs independently on Tokio runtime)
3. **teardown()**: Signal shutdown, wait for graceful termination

```rust
impl crate::core::Processor for ApiServerProcessor::Processor {
    fn setup(&mut self, ctx: &RuntimeContext) -> Result<()> {
        // Get the service for communicating with runtime
        self.runtime_service = Some(ctx.runtime_service());

        let config = self.config.clone();
        let service = self.runtime_service.clone().unwrap();
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();

        // Spawn Tokio runtime on background thread
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .expect("Failed to create Tokio runtime");

            rt.block_on(async move {
                run_http_server(config, service, shutdown_rx).await;
            });
        });

        self.shutdown_tx = Some(shutdown_tx);
        tracing::info!("API server starting on {}:{}", config.host, config.port);
        Ok(())
    }

    fn process(&mut self) -> Result<()> {
        // Server runs independently on Tokio runtime
        Ok(())
    }

    fn teardown(&mut self) -> Result<()> {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
            tracing::info!("API server shutdown signal sent");
        }
        Ok(())
    }
}
```

---

## HTTP Server (Axum)

```rust
async fn run_http_server(
    config: ApiServerConfig,
    service: RuntimeService,
    mut shutdown_rx: tokio::sync::oneshot::Receiver<()>,
) {
    let app = Router::new()
        .route("/api/processors", post(create_processor).get(list_processors))
        .route("/api/processors/:id", get(get_processor).delete(remove_processor))
        .route("/api/connections", post(create_connection).get(list_connections))
        .route("/api/connections/:id", delete(remove_connection))
        .route("/api/runtime/start", post(start_runtime))
        .route("/api/runtime/stop", post(stop_runtime))
        .route("/api/runtime/state", get(get_state))
        .route("/api/events", get(websocket_handler))
        .with_state(service);

    let addr = format!("{}:{}", config.host, config.port);
    let listener = tokio::net::TcpListener::bind(&addr).await.unwrap();

    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            let _ = shutdown_rx.await;
        })
        .await
        .unwrap();
}
```

### Example Handler

```rust
async fn create_processor(
    State(service): State<RuntimeService>,
    Json(request): Json<CreateProcessorRequest>,
) -> Result<Json<CreateProcessorResponse>, AppError> {
    // Use the factory to create processor from type name
    let spec = PROCESSOR_REGISTRY.create(&request.processor_type, request.config)?;

    // Send command to main thread, await response
    let processor_id = service.add_processor(spec).await?;

    Ok(Json(CreateProcessorResponse { processor_id }))
}
```

---

## HTTP API Endpoints

### Processors

```
POST   /api/processors          Create processor
GET    /api/processors          List processors
GET    /api/processors/:id      Get processor details
DELETE /api/processors/:id      Remove processor
```

### Connections

```
POST   /api/connections         Create connection
GET    /api/connections         List connections
DELETE /api/connections/:id     Remove connection
```

### Lifecycle

```
POST   /api/runtime/start       Start runtime
POST   /api/runtime/stop        Stop runtime
GET    /api/runtime/state       Get runtime state
```

### WebSocket

```
WS     /api/events              Subscribe to runtime events
```

---

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

---

## Files to Create

### Service Layer (in `libs/streamlib/src/core/service/`)

1. `mod.rs` - Module exports
2. `command.rs` - RuntimeCommand, CommandResult (internal)
3. `runtime_service.rs` - RuntimeService (public API)

### Processor

4. `libs/streamlib/src/core/processors/api_server.rs` - ApiServerProcessor

### HTTP Server (feature-gated)

5. `libs/streamlib/src/core/service/http/mod.rs` - Axum server setup
6. `libs/streamlib/src/core/service/http/handlers.rs` - Route handlers
7. `libs/streamlib/src/core/service/http/websocket.rs` - WebSocket event streaming

---

## Implementation Order

1. **Service layer foundation**
   - `RuntimeCommand` and `CommandResult` enums
   - `RuntimeService` struct with async methods

2. **Runtime integration**
   - Add command channel to `StreamRuntime`
   - Add `poll_commands()` method
   - Add `runtime_service()` to `RuntimeContext`

3. **ApiServerProcessor**
   - Fix existing skeleton
   - Implement `setup()`, `process()`, `teardown()`
   - Spawn Tokio runtime

4. **HTTP server**
   - Axum router setup
   - Handlers using `RuntimeService`
   - WebSocket event streaming

---

## Open Questions

1. **Feature flag**: Should the API server be behind a feature flag (e.g., `api-server`)?
2. **WebSocket filtering**: Should clients subscribe to specific event types?
3. **Authentication**: Add auth middleware for production use?
4. **Rate limiting**: Protect against abuse?
