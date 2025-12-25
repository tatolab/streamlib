# RFC: API Server Processor

## Status: Draft

## Summary

Build an `ApiServerProcessor` - a ManualProcessor that runs an Axum HTTP/WebSocket server, exposing StreamRuntime control via REST API. This enables building web UIs for StreamLib.

## Motivation

StreamLib needs a way for external applications (web UIs, control panels, automation tools) to:
- Create/remove processors dynamically
- Connect/disconnect ports
- Query runtime state (graph topology, processor status)
- Subscribe to real-time events via WebSocket

## Design

### Architecture

```
┌─────────────────────────────────────────────────────────────────┐
│  StreamRuntime                                                  │
│                                                                 │
│  ┌──────────────────────────────────────────────────────────┐  │
│  │  ApiServerProcessor (ManualProcessor)                     │  │
│  │                                                           │  │
│  │  setup(): Store RuntimeContext                            │  │
│  │  start(): Spawn Axum server on ctx.tokio_handle()         │  │
│  │  stop(): Signal shutdown, wait for server to stop         │  │
│  │  teardown(): Cleanup                                      │  │
│  │                                                           │  │
│  │  ┌─────────────────────────────────────────────────────┐ │  │
│  │  │  Axum Server (runs on shared tokio runtime)         │ │  │
│  │  │  - REST endpoints call ctx.runtime().*_async()      │ │  │
│  │  │  - WebSocket broadcasts events to clients           │ │  │
│  │  └─────────────────────────────────────────────────────┘ │  │
│  └──────────────────────────────────────────────────────────┘  │
│                                                                 │
│  ┌──────────────────────────────────────────────────────────┐  │
│  │  Other Processors (Camera, Display, etc.)                 │  │
│  └──────────────────────────────────────────────────────────┘  │
└─────────────────────────────────────────────────────────────────┘
```

### Key Design Decisions

1. **Use shared tokio runtime** - Spawn server task via `ctx.tokio_handle().spawn()`. No separate thread.

2. **Async RuntimeOperations** - HTTP handlers call `ctx.runtime().add_processor_async().await` directly.

3. **Event broadcasting** - Use `tokio::sync::broadcast` channel to fan out events to WebSocket clients.

4. **Graceful shutdown** - Use `tokio::sync::oneshot` to signal server shutdown from `stop()`.

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
| GET | `/api/runtime/state` | Get runtime state (graph topology) |
| GET | `/health` | Health check |

### WebSocket

| Endpoint | Description |
|----------|-------------|
| `/api/events` | Subscribe to runtime events (JSON stream) |

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
│   ├── server.rs        # Axum server setup, routes
│   ├── handlers.rs      # REST endpoint handlers
│   ├── websocket.rs     # WebSocket handler, event streaming
│   └── error.rs         # AppError, response mapping
```

## Dependencies

```toml
[dependencies]
axum = { version = "0.8", features = ["ws"], optional = true }
tower-http = { version = "0.6", features = ["cors"], optional = true }

[features]
default = []
api-server = ["axum", "tower-http"]
```

## Implementation Checklist

- [ ] Add `axum` and `tower-http` to Cargo.toml (feature-gated)
- [ ] Create `api_server/mod.rs` with processor definition
- [ ] Implement `ManualProcessor` lifecycle (setup/start/stop/teardown)
- [ ] Create `api_server/server.rs` with Axum router and graceful shutdown
- [ ] Create `api_server/handlers.rs` with REST endpoints
- [ ] Create `api_server/websocket.rs` with event streaming
- [ ] Create `api_server/error.rs` with StreamError → HTTP status mapping
- [ ] Add integration tests
- [ ] Create example: `examples/api-server-demo/`
