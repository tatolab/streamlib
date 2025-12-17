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
│  - StreamRuntime                                            │
│  - Graph mutations                                          │
│  - macOS: NSApplication event loop                          │
│  - Reads commands from RuntimeCommandService                │
└─────────────────────────────────────────────────────────────┘
                           ▲
                           │ commands via channel
┌──────────────────────────┴──────────────────────────────────┐
│  TOKIO RUNTIME (background threads)                         │
│  - ApiServerProcessor runs HTTP/WS server                   │
│  - Sends RuntimeCommand to main thread                      │
│  - Receives events, broadcasts to WebSocket clients         │
└─────────────────────────────────────────────────────────────┘
```

### Why This Architecture

Apple frameworks (AVFoundation, VideoToolbox, CoreMedia) require main thread execution. Tokio must be isolated to background threads to avoid blocking hardware access.

---

## RuntimeCommandService

Thread-safe bridge between API server (Tokio) and main thread (runtime).

```rust
#[derive(Clone)]
pub struct RuntimeCommandClient {
    command_tx: flume::Sender<(RuntimeCommand, oneshot::Sender<CommandResult>)>,
    event_rx: broadcast::Receiver<Event>,
}

impl RuntimeCommandClient {
    pub async fn send(&self, command: RuntimeCommand) -> Result<CommandResult>;
    pub fn subscribe_events(&self) -> broadcast::Receiver<Event>;
}

pub struct RuntimeCommandServer {
    command_rx: flume::Receiver<(RuntimeCommand, oneshot::Sender<CommandResult>)>,
    event_tx: broadcast::Sender<Event>,
}

impl RuntimeCommandServer {
    /// Called from main thread event loop
    pub fn poll(&self, runtime: &mut StreamRuntime) -> Option<CommandResult>;
}
```

### RuntimeCommand Enum

```rust
pub enum RuntimeCommand {
    AddProcessor {
        node: ProcessorNode,
    },
    RemoveProcessor {
        processor_id: ProcessorUniqueId,
    },
    Connect {
        from: OutputLinkPortRef,
        to: InputLinkPortRef,
    },
    Disconnect {
        link_id: LinkUniqueId,
    },
    Start,
    Stop,
    GetState,
}

pub enum CommandResult {
    ProcessorAdded { processor_id: ProcessorUniqueId },
    ProcessorRemoved,
    Connected { link_id: LinkUniqueId },
    Disconnected,
    Started,
    Stopped,
    State { /* graph state */ },
    Error(StreamError),
}
```

---

## ApiServerProcessor

API server as a processor type, fitting the streamlib pattern.

```rust
#[streamlib::processor(
    execution = Spawned,
    description = "HTTP/WebSocket API server"
)]
pub struct ApiServerProcessor {
    #[streamlib::config]
    config: ApiServerConfig,

    command_client: RuntimeCommandClient,
    tokio_handle: Option<tokio::runtime::Handle>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiServerConfig {
    pub host: String,
    pub port: u16,
}
```

### Lifecycle

1. **on_start**: Spawn Tokio runtime on background thread, start HTTP server
2. **on_process**: No-op (server runs independently)
3. **on_stop**: Shutdown Tokio runtime gracefully

```rust
impl ApiServerProcessor {
    fn on_start(&mut self, ctx: &RuntimeContext) -> Result<()> {
        let config = self.config.clone();
        let client = self.command_client.clone();

        let (handle_tx, handle_rx) = std::sync::mpsc::channel();

        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .unwrap();

            handle_tx.send(rt.handle().clone()).ok();

            rt.block_on(async move {
                run_http_server(config, client).await;
            });
        });

        self.tokio_handle = Some(handle_rx.recv()?);
        Ok(())
    }
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

1. `libs/streamlib/src/core/service/mod.rs` - RuntimeCommandService
2. `libs/streamlib/src/core/service/command.rs` - RuntimeCommand enum
3. `libs/streamlib/src/core/service/client.rs` - RuntimeCommandClient
4. `libs/streamlib/src/core/service/server.rs` - RuntimeCommandServer
5. `libs/streamlib/src/core/processors/api_server.rs` - ApiServerProcessor

---

## Open Questions

1. Should ApiServerProcessor be in core or a separate `streamlib-api` crate?
2. WebSocket event filtering - should clients subscribe to specific event types?
3. Authentication/authorization for API endpoints?
4. Rate limiting for API requests?
