# Lab: Building an API Server for StreamRuntime

## Status: Draft (Lab Format)

## Overview

This lab teaches you how to build a thread-safe API server that controls StreamRuntime from HTTP/WebSocket clients. You'll learn:

- **Tokio**: Async runtime fundamentals
- **Axum**: HTTP server and routing
- **Channels**: Cross-thread communication patterns
- **RuntimeProxy**: Async facade over sync runtime

By the end, you'll understand how to expose StreamRuntime operations via REST API while ensuring the runtime never blocks.

---

## Prerequisites Completed

The following foundational changes have already been made to enable this lab:

### 1. Interior Mutability Refactor ✅

StreamRuntime methods now take `&self` instead of `&mut self`, enabling concurrent access:

```rust
// OLD - required exclusive mutable access
pub fn add_processor(&mut self, spec: ProcessorSpec) -> Result<ProcessorUniqueId>

// NEW - allows shared access via Arc<StreamRuntime>
pub fn add_processor(&self, spec: ProcessorSpec) -> Result<ProcessorUniqueId>
```

**Why this matters**: The RuntimeProxy command handler can call these methods without needing exclusive access. Multiple commands can be processed concurrently.

**Internal locking strategy**:
- `status: Mutex<RuntimeStatus>` - lifecycle state
- `runtime_context: OnceLock<Arc<RuntimeContext>>` - set once at start()
- Graph operations use `Arc<RwLock<Graph>>` via Compiler
- Pending operations use `Arc<Mutex<Vec<PendingOperation>>>`

### 2. Compiler Main Thread Dispatch Removed ✅

The compiler no longer forces all compilation to the main thread:

```rust
// OLD - forced everything to main thread
runtime_ctx.run_on_main_blocking(move || {
    Self::compile(...)
})

// NEW - processors handle their own main thread needs
Self::compile(...)
```

**Why this matters**: Only Apple framework processors (Camera, Display) need main thread. They call `ctx.run_on_main_blocking()` in their own `setup()`. This keeps the compiler simple and allows non-Apple processors to run without main thread constraints.

### 3. Cross-Platform Main Thread Documentation ✅

`RuntimeContext` now has documented stubs for Windows and Linux:

- **macOS**: Uses `dispatch2::DispatchQueue::main()` (implemented)
- **Windows**: TODO - `PostMessage` to Win32 message loop
- **Linux**: TODO - `glib::MainContext` or eventfd
- **Other**: Passthrough (executes directly)

---

## Architecture Overview

```
┌─────────────────────────────────────────────────────────────────┐
│  StreamRuntime (single instance)                                │
│                                                                 │
│  ┌──────────────────────────────────────────────────────────┐  │
│  │  ApiServerProcessor                                       │  │
│  │  - Runs HTTP/WS server on Tokio (background thread)       │  │
│  │  - Uses RuntimeProxy to send commands                     │  │
│  └──────────────────────────────────────────────────────────┘  │
│                              │                                  │
│                              │ RuntimeCommand via flume channel │
│                              │ CommandResult via oneshot        │
│                              ▼                                  │
│  ┌──────────────────────────────────────────────────────────┐  │
│  │  Command Handler (polls channel)                          │  │
│  │  - Calls runtime.add_processor(), connect(), etc.         │  │
│  │  - Sends result back via oneshot                          │  │
│  └──────────────────────────────────────────────────────────┘  │
└─────────────────────────────────────────────────────────────────┘
```

### Key Insight: RuntimeProxy is NOT a Duplicate

The proxy is a **cross-thread communication facade**, not a duplicate of StreamRuntime:

1. HTTP handler calls `proxy.add_processor(spec).await`
2. Proxy serializes into `RuntimeCommand::AddProcessor`
3. Command sent over channel
4. Handler receives command, calls `runtime.add_processor(spec)` (the real implementation)
5. Result sent back via oneshot channel

The proxy contains **zero business logic**.

---

## Lab 1: Understanding the Problem

### Why Can't We Just Use Arc<StreamRuntime>?

With the `&self` refactor, you might think we can do:

```rust
let runtime = Arc::new(StreamRuntime::new()?);
let runtime_clone = Arc::clone(&runtime);

tokio::spawn(async move {
    runtime_clone.add_processor(spec); // This works syntactically...
});
```

**Problem**: `add_processor()` is a **sync** function. Calling it from async context:
- Blocks the Tokio worker thread
- Prevents other async tasks from running
- Can cause deadlocks if the operation takes time

### Why Not Make add_processor() Async?

StreamRuntime is intentionally **synchronous**:
- Apple frameworks require main thread execution
- GPU operations have specific threading requirements
- Simpler mental model for processor authors

### The Solution: Channel-Based Proxy

Decouple the async world (Tokio/Axum) from the sync world (StreamRuntime):

```
Async World                    Sync World
┌─────────────┐               ┌─────────────┐
│ Axum Handler │──command──▶  │ StreamRuntime│
│   (async)   │◀──result───  │   (sync)    │
└─────────────┘               └─────────────┘
        │                           │
     Tokio                    Main Thread
```

---

## Lab 2: Channel Fundamentals

### Types of Channels

| Channel Type | Use Case | Crate |
|--------------|----------|-------|
| **mpsc** | Multiple producers, single consumer | `flume`, `tokio::sync` |
| **oneshot** | Single value, one-time response | `tokio::sync::oneshot` |
| **broadcast** | Multiple consumers, all receive | `tokio::sync::broadcast` |

### Why flume for Commands?

```rust
// flume works in both sync and async contexts
let (tx, rx) = flume::unbounded::<RuntimeCommand>();

// Async send (from Tokio)
tx.send_async(command).await?;

// Sync receive (from main thread)
while let Ok(command) = rx.try_recv() {
    // process command
}
```

`flume` is a multi-producer, multi-consumer channel that bridges sync/async seamlessly. Unlike `tokio::sync::mpsc`, it doesn't require a Tokio runtime to function.

### Request-Response Pattern

```rust
// Sender side (async)
let (response_tx, response_rx) = tokio::sync::oneshot::channel();
command_tx.send_async((command, response_tx)).await?;
let result = response_rx.await?;  // Wait for response

// Receiver side (sync)
let (command, response_tx) = command_rx.recv()?;
let result = execute_command(command);
response_tx.send(result).ok();  // Send response back
```

---

## Lab 3: RuntimeProxy Design

### Command and Result Types

```rust
/// Commands sent from async world to sync runtime.
/// Internal to the service layer - not exposed publicly.
pub(crate) enum RuntimeCommand {
    AddProcessor { spec: ProcessorSpec },
    RemoveProcessor { id: ProcessorUniqueId },
    Connect { from: OutputLinkPortRef, to: InputLinkPortRef },
    Disconnect { link_id: LinkUniqueId },
    Start,
    Stop,
    GetState,
}

/// Results returned from runtime to async caller.
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

### RuntimeProxy Implementation

```rust
/// Async facade for StreamRuntime operations.
///
/// Obtained from `RuntimeContext::runtime_proxy()` in processor setup.
/// All methods are async and safe to call from Tokio.
#[derive(Clone)]
pub struct RuntimeProxy {
    command_tx: flume::Sender<(RuntimeCommand, oneshot::Sender<CommandResult>)>,
    event_tx: broadcast::Sender<RuntimeEvent>,
}

impl RuntimeProxy {
    /// Add a processor to the runtime.
    pub async fn add_processor(&self, spec: ProcessorSpec) -> Result<ProcessorUniqueId> {
        let (response_tx, response_rx) = oneshot::channel();

        self.command_tx
            .send_async((RuntimeCommand::AddProcessor { spec }, response_tx))
            .await
            .map_err(|_| StreamError::Runtime("Command channel closed".into()))?;

        match response_rx.await {
            Ok(CommandResult::ProcessorAdded(id)) => Ok(id),
            Ok(CommandResult::Error(e)) => Err(e),
            Ok(_) => Err(StreamError::Runtime("Unexpected response".into())),
            Err(_) => Err(StreamError::Runtime("Response channel closed".into())),
        }
    }

    /// Subscribe to runtime events (for WebSocket broadcasting).
    pub fn subscribe_events(&self) -> broadcast::Receiver<RuntimeEvent> {
        self.event_tx.subscribe()
    }

    // Similar implementations for connect(), disconnect(), start(), stop(), etc.
}
```

### How Processors Obtain RuntimeProxy

```rust
impl crate::core::Processor for ApiServerProcessor::Processor {
    fn setup(&mut self, ctx: &RuntimeContext) -> Result<()> {
        // RuntimeContext provides the proxy
        self.runtime_proxy = Some(ctx.runtime_proxy());
        Ok(())
    }
}
```

---

## Lab 4: StreamRuntime Integration

### Adding Command Channel to Runtime

```rust
pub struct StreamRuntime {
    pub(crate) compiler: Compiler,
    pub(crate) runtime_context: OnceLock<Arc<RuntimeContext>>,
    pub(crate) status: Mutex<RuntimeStatus>,

    // NEW: Command channel for RuntimeProxy
    command_rx: flume::Receiver<(RuntimeCommand, oneshot::Sender<CommandResult>)>,
    runtime_proxy: RuntimeProxy,  // Cloneable, given to processors
}

impl StreamRuntime {
    pub fn new() -> Result<Self> {
        // ... existing initialization ...

        // Create command channel
        let (command_tx, command_rx) = flume::unbounded();
        let (event_tx, _) = broadcast::channel(256);

        let runtime_proxy = RuntimeProxy { command_tx, event_tx };

        Ok(Self {
            compiler: Compiler::new(),
            runtime_context: OnceLock::new(),
            status: Mutex::new(RuntimeStatus::Initial),
            command_rx,
            runtime_proxy,
        })
    }
}
```

### Polling Commands

The runtime polls for commands in its event loop:

```rust
impl StreamRuntime {
    /// Poll and execute pending commands.
    /// Call this from your main loop or integrate with platform event loop.
    pub fn poll_commands(&self) {
        while let Ok((command, response_tx)) = self.command_rx.try_recv() {
            let result = self.execute_command(command);
            let _ = response_tx.send(result);
        }
    }

    fn execute_command(&self, command: RuntimeCommand) -> CommandResult {
        match command {
            RuntimeCommand::AddProcessor { spec } => {
                match self.add_processor(spec) {
                    Ok(id) => CommandResult::ProcessorAdded(id),
                    Err(e) => CommandResult::Error(e),
                }
            }
            RuntimeCommand::Connect { from, to } => {
                match self.connect(from, to) {
                    Ok(id) => CommandResult::Connected(id),
                    Err(e) => CommandResult::Error(e),
                }
            }
            RuntimeCommand::Start => {
                match self.start() {
                    Ok(()) => CommandResult::Started,
                    Err(e) => CommandResult::Error(e),
                }
            }
            // ... other commands ...
        }
    }
}
```

### RuntimeContext Enhancement

```rust
impl RuntimeContext {
    /// Get a RuntimeProxy for cross-thread runtime control.
    ///
    /// Use this in processors that need to modify the graph dynamically
    /// (e.g., API servers, orchestration processors).
    pub fn runtime_proxy(&self) -> RuntimeProxy {
        self.runtime_proxy.clone()
    }
}
```

---

## Lab 5: Tokio Runtime Isolation

### Why Isolate Tokio?

StreamRuntime runs on the main thread (required for macOS). Tokio wants to run its own thread pool. Solution: spawn Tokio on a background thread.

```rust
impl crate::core::Processor for ApiServerProcessor::Processor {
    fn setup(&mut self, ctx: &RuntimeContext) -> Result<()> {
        self.runtime_proxy = Some(ctx.runtime_proxy());

        let config = self.config.clone();
        let proxy = self.runtime_proxy.clone().unwrap();
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();

        // Spawn Tokio on a background thread - NOT the main thread
        std::thread::spawn(move || {
            // Build a new Tokio runtime for this thread
            let rt = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .expect("Failed to create Tokio runtime");

            // block_on runs the async server until completion
            rt.block_on(async move {
                run_http_server(config, proxy, shutdown_rx).await;
            });
        });

        self.shutdown_tx = Some(shutdown_tx);
        Ok(())
    }

    fn teardown(&mut self) -> Result<()> {
        // Signal shutdown to the Tokio runtime
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
        Ok(())
    }
}
```

### Key Points

1. **`std::thread::spawn`** - Creates an OS thread separate from main
2. **`tokio::runtime::Builder`** - Creates a new Tokio runtime on that thread
3. **`rt.block_on`** - Runs async code, blocking the thread until complete
4. **Shutdown via oneshot** - Clean signal to stop the server

---

## Lab 6: Axum HTTP Server

### Router Setup

```rust
use axum::{
    routing::{get, post, delete},
    Router,
    extract::{State, Path, Json},
};

async fn run_http_server(
    config: ApiServerConfig,
    proxy: RuntimeProxy,
    shutdown_rx: oneshot::Receiver<()>,
) {
    let app = Router::new()
        // Processor endpoints
        .route("/api/processors", post(create_processor).get(list_processors))
        .route("/api/processors/:id", get(get_processor).delete(remove_processor))
        // Connection endpoints
        .route("/api/connections", post(create_connection).get(list_connections))
        .route("/api/connections/:id", delete(remove_connection))
        // Lifecycle endpoints
        .route("/api/runtime/start", post(start_runtime))
        .route("/api/runtime/stop", post(stop_runtime))
        .route("/api/runtime/state", get(get_state))
        // WebSocket for events
        .route("/api/events", get(websocket_handler))
        // Attach the proxy as shared state
        .with_state(proxy);

    let addr = format!("{}:{}", config.host, config.port);
    let listener = tokio::net::TcpListener::bind(&addr).await.unwrap();

    tracing::info!("API server listening on {}", addr);

    // Graceful shutdown when shutdown_rx receives signal
    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            let _ = shutdown_rx.await;
            tracing::info!("API server shutting down");
        })
        .await
        .unwrap();
}
```

### Handler Implementation

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
    State(proxy): State<RuntimeProxy>,
    Json(request): Json<CreateProcessorRequest>,
) -> Result<Json<CreateProcessorResponse>, AppError> {
    // Create ProcessorSpec from request
    let spec = ProcessorSpec::new(&request.processor_type, request.config);

    // Send command to runtime via proxy, await response
    let processor_id = proxy.add_processor(spec).await?;

    Ok(Json(CreateProcessorResponse {
        processor_id: processor_id.to_string(),
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

---

## Lab 7: WebSocket Event Streaming

### Broadcast Pattern

```rust
use axum::extract::ws::{WebSocket, WebSocketUpgrade, Message};

async fn websocket_handler(
    ws: WebSocketUpgrade,
    State(proxy): State<RuntimeProxy>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_websocket(socket, proxy))
}

async fn handle_websocket(mut socket: WebSocket, proxy: RuntimeProxy) {
    // Subscribe to runtime events
    let mut event_rx = proxy.subscribe_events();

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
                    Err(broadcast::error::RecvError::Lagged(_)) => {
                        // Client too slow, some events dropped
                        continue;
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        break; // Channel closed
                    }
                }
            }

            // Handle incoming WebSocket messages (if needed)
            msg = socket.recv() => {
                match msg {
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Ok(Message::Ping(data))) => {
                        let _ = socket.send(Message::Pong(data)).await;
                    }
                    _ => {} // Ignore other messages
                }
            }
        }
    }
}
```

---

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

### Lifecycle

| Method | Endpoint | Description |
|--------|----------|-------------|
| POST | `/api/runtime/start` | Start runtime |
| POST | `/api/runtime/stop` | Stop runtime |
| GET | `/api/runtime/state` | Get runtime state |

### WebSocket

| Endpoint | Description |
|----------|-------------|
| `/api/events` | Subscribe to runtime events |

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

## Implementation Checklist

### Files to Create

**Service Layer** (`libs/streamlib/src/core/service/`):
- [ ] `mod.rs` - Module exports
- [ ] `command.rs` - RuntimeCommand, CommandResult
- [ ] `runtime_proxy.rs` - RuntimeProxy

**Runtime Integration**:
- [ ] Update `StreamRuntime` with command channel
- [ ] Add `poll_commands()` method
- [ ] Update `RuntimeContext` with `runtime_proxy()`

**HTTP Server** (feature-gated: `api-server`):
- [ ] `libs/streamlib/src/core/service/http/mod.rs` - Server setup
- [ ] `libs/streamlib/src/core/service/http/handlers.rs` - Route handlers
- [ ] `libs/streamlib/src/core/service/http/websocket.rs` - WebSocket handling

**Processor**:
- [ ] Complete `libs/streamlib/src/core/processors/api_server.rs`

### Dependencies to Add

```toml
[dependencies]
# Channel for sync/async bridging
flume = "0.11"

# HTTP server (feature-gated)
axum = { version = "0.8", optional = true }
tokio = { version = "1", features = ["full"], optional = true }

[features]
api-server = ["axum", "tokio"]
```

---

## Open Questions

1. **Feature flag**: Should require `--features api-server` to include HTTP dependencies?
2. **WebSocket filtering**: Should clients subscribe to specific event types?
3. **Authentication**: Add auth middleware for production?
4. **Rate limiting**: Protect against API abuse?
5. **CORS**: Configure for browser clients?
