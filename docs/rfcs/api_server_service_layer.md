# Lab: Building an API Server for StreamRuntime

## Status: Draft (Lab Format)

---

<mentor-guidance>
## For the Mentor Agent

**When the developer references this document, acknowledge it explicitly:**

> "I see you're working with the API Server lab. I've read through the complete implementation plan that the Lab Builder prepared. This is a well-structured 7-lab progression covering channels, RuntimeProxy, Tokio isolation, Axum, and WebSockets. I'm ready to guide you through it - you'll write the code, and I'll help you arrive at solutions that match the reference implementations. Where would you like to start?"

### Your Role
You are guiding an experienced developer (20+ years) who is learning Rust. The Lab Builder agent has prepared complete reference implementations in this document. Your job:
- **Guide, don't solve** - ask leading questions
- **Validate against the code blocks** - they are the answer key
- **Let them write the code** - only show solutions if explicitly asked
- **Celebrate their wins** - they're learning a new language

### What This Lab Contains
- **7 progressive labs** with complete, working implementations
- Each lab explains **"why"** before showing **"how"**
- Code blocks are your **answer key** - validate their work against these
- The developer should arrive at solutions that match (or improve upon) these

### Branch State (`api/server`)
The Lab Builder has already completed foundational pre-work (5 items):
1. ✅ Interior mutability (`&self` not `&mut self`)
2. ✅ Compiler simplified (no forced main thread dispatch)
3. ✅ Runtime restart support (`Mutex<Option<...>>`)
4. ✅ Event-driven commit system (`GraphChangeListener` + PUBSUB)
5. ✅ Dependencies in Cargo.toml (tokio, axum, tower-http)

**The developer does NOT need to implement these** - they're documented in "Prerequisites Completed" for context.

**Key insight from Prerequisite 4**: Graph operations are now guaranteed fast and deadlock-free. The `RuntimeOperations` trait pattern (documented after Architecture Overview) enables a unified API across all contexts.

### Target Files (What They'll Create/Modify)
| File | Action | Lab |
|------|--------|-----|
| `libs/streamlib/src/core/service/mod.rs` | Create | 3 |
| `libs/streamlib/src/core/service/command.rs` | Create | 3 |
| `libs/streamlib/src/core/service/runtime_proxy.rs` | Create | 3 |
| `libs/streamlib/src/core/runtime/runtime.rs` | Modify | 4 |
| `libs/streamlib/src/core/context/runtime_context.rs` | Modify | 4 |
| `libs/streamlib/src/core/service/http/mod.rs` | Create | 5-6 |
| `libs/streamlib/src/core/service/http/handlers.rs` | Create | 6 |
| `libs/streamlib/src/core/service/http/websocket.rs` | Create | 7 |
| `libs/streamlib/src/core/processors/api_server.rs` | Complete | 5 |

### The Story Arc
This lab tells a story of **bridging two worlds**:

1. **The Problem** (Lab 1): StreamRuntime is sync, Axum is async. They can't talk directly.
2. **The Bridge** (Labs 2-4): Channels create a message-passing bridge between worlds.
3. **The Isolation** (Lab 5): Tokio gets its own thread, away from macOS main thread constraints.
4. **The Interface** (Labs 6-7): HTTP and WebSocket expose the runtime to the outside world.

The developer should understand this narrative - it's not just code, it's architecture.

### Key Concepts to Reinforce
| Concept | Why It Matters | When They'll Encounter It |
|---------|----------------|---------------------------|
| `try_recv()` takes `&mut self` | Requires `Mutex` wrapper | Lab 2, 4 |
| Tokio on background thread | macOS main thread is for Apple frameworks | Lab 5 |
| `oneshot` for request-response | Each command gets exactly one response | Lab 2, 3 |
| `broadcast` for fan-out | Multiple WebSocket clients, all get events | Lab 2, 7 |
| Proxy has ZERO logic | It's just a channel facade, not business logic | Lab 3 |
| `clone()` for async moves | Values move into async blocks | Lab 5, 6 |
| `RuntimeOperations` trait | Unified API across all contexts | Architecture |
| Event-driven commit | Graph ops return immediately, no deadlock | Prerequisite 4 |

### Common Stumbling Points
When they hit these, guide with questions, not answers:

1. **"Why do I need Mutex around the receiver?"**
   - Ask: "What does `try_recv()`'s signature require?"
   - Hint: Look at whether it takes `&self` or `&mut self`

2. **"My Tokio code deadlocks on macOS"**
   - Ask: "Which thread is Tokio running on? Which thread does macOS need free?"
   - Hint: `std::thread::spawn` vs calling `block_on` directly

3. **"How do I get the proxy into my handler?"**
   - Ask: "How does Axum share state across handlers?"
   - Hint: `.with_state()` and `State` extractor

4. **"The oneshot channel closed unexpectedly"**
   - Ask: "Who holds the sender? What happens when they drop it?"
   - Hint: Trace the sender's lifetime

### Verification Checkpoints
Use these to confirm progress:

| After Lab | Test | Expected |
|-----------|------|----------|
| 5 | `curl http://localhost:9000/health` | `ok` |
| 6 | `curl http://localhost:9000/api/runtime/state` | `{"status":"running"}` |
| 6 | `curl -X POST .../api/processors -d '{"processor_type":"..."}` | `{"processor_id":"..."}` |
| 7 | `websocat ws://localhost:9000/api/events` | Events stream when processors added |

### Code Review Focus
When reviewing their implementations:

- **Error handling**: Are they using `?` properly? Mapping errors to `AppError`?
- **Ownership**: Did they `clone()` what needs to move into async blocks?
- **Lifetimes**: The `CommandMessage` type alias avoids embedding lifetimes in the tuple
- **Mutex scope**: Are they holding locks longer than necessary? (Lock, extract, drop)
- **Match exhaustiveness**: Did they handle all `CommandResult` variants?
</mentor-guidance>

---

## Overview

This lab teaches you how to build a thread-safe API server that controls StreamRuntime from HTTP/WebSocket clients. You'll learn:

- **Tokio**: Async runtime fundamentals
- **Axum**: HTTP server and routing
- **Channels**: Cross-thread communication patterns
- **RuntimeProxy**: Async facade over sync runtime

By the end, you'll understand how to expose StreamRuntime operations via REST API while ensuring the runtime never blocks.

> **🎯 The Goal**: A web application can call `POST /api/processors` to create a camera, `POST /api/connections` to wire it to a display, and receive real-time events via WebSocket when things change. The runtime stays responsive, the API stays async, and they never block each other.

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
- `runtime_context: Mutex<Option<Arc<RuntimeContext>>>` - created on start(), cleared on stop()
- Graph operations use `Arc<RwLock<Graph>>` via Compiler
- Pending operations use `Arc<Mutex<Vec<PendingOperation>>>`

### 2. Compiler Main Thread Dispatch Removed ✅

The compiler no longer forces all compilation to the runtime thread:

```rust
// OLD - forced everything to runtime thread
runtime_ctx.run_on_runtime_thread_blocking(move || {
    Self::compile(...)
})

// NEW - processors handle their own runtime thread needs
Self::compile(...)
```

**Why this matters**: Only Apple framework processors (Camera, Display) need runtime thread. They call `ctx.run_on_runtime_thread_blocking()` in their own `setup()`. This keeps the compiler simple and allows non-Apple processors to run without runtime thread constraints.

### 3. Cross-Platform Main Thread Documentation ✅

`RuntimeContext` now has documented stubs for Windows and Linux:

- **macOS**: Uses `dispatch2::DispatchQueue::main()` (implemented)
- **Windows**: TODO - `PostMessage` to Win32 message loop
- **Linux**: TODO - `glib::MainContext` or eventfd
- **Other**: Passthrough (executes directly)

### 4. Event-Driven Commit System ✅

Graph operations (`add_processor`, `connect`, `disconnect`, `remove_processor`) now return **immediately**. Compilation is decoupled via the PUBSUB event system:

```rust
// OLD - graph operation blocked on compilation
pub fn add_processor(&self, spec: ProcessorSpec) -> Result<ProcessorUniqueId> {
    // ... add to graph ...
    self.commit()?;  // ← Blocked here during compilation!
    Ok(processor_id)
}

// NEW - graph operation returns immediately, commit is async
pub fn add_processor(&self, spec: ProcessorSpec) -> Result<ProcessorUniqueId> {
    // ... add to graph ...
    PUBSUB.publish(topics::RUNTIME_GLOBAL, &Event::RuntimeGlobal(RuntimeEvent::GraphDidChange));
    Ok(processor_id)  // ← Returns immediately!
}
```

**How it works:**
1. `GraphChangeListener` subscribes to `GraphDidChange` events in `StreamRuntime::new()`
2. When event received, listener dispatches commit to runtime thread via `run_on_runtime_thread_async`
3. Commit happens asynchronously, interleaved with the event loop
4. Before runtime starts, commits are deferred to `start()`

**Why this matters for the API server:**
- `poll_commands()` stays fast - never blocks on compilation
- `execute_command()` can call any graph operation without deadlock risk
- Multiple rapid API calls queue up gracefully; compiler batches operations

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
│                              │ RuntimeCommand via mpsc channel  │
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

## Unified API Pattern: RuntimeOperations Trait

> **🎯 Design Goal**: Whether you're in `examples/camera-display` calling `runtime.add_processor()`, inside a processor calling `ctx.runtime().add_processor()`, or in an async Axum handler - the API should be **identical**. The caller doesn't know or care whether they're going through a proxy or calling directly.

### The Problem with Multiple APIs

Without unification, callers need to know their context:

```rust
// External code - direct call
runtime.add_processor(spec)?;

// Inside a processor - need proxy? direct? channel?
ctx.???_add_processor(spec)?;

// Async handler - definitely need proxy
proxy.add_processor(spec).await?;
```

### The Solution: RuntimeOperations Trait

Define a trait that both `StreamRuntime` and `RuntimeProxy` implement:

```rust
/// Unified interface for runtime graph operations.
///
/// Implemented by both StreamRuntime (sync, direct) and RuntimeProxy (via channels).
/// Callers use this trait and don't need to know the underlying implementation.
pub trait RuntimeOperations {
    fn add_processor(&self, spec: ProcessorSpec) -> Result<ProcessorUniqueId>;
    fn remove_processor(&self, id: &ProcessorUniqueId) -> Result<()>;
    fn connect(&self, from: OutputLinkPortRef, to: InputLinkPortRef) -> Result<LinkUniqueId>;
    fn disconnect(&self, link_id: &LinkUniqueId) -> Result<()>;
    fn update_processor_config<C: Serialize>(&self, id: &ProcessorUniqueId, config: C) -> Result<()>;
}
```

### Implementation: StreamRuntime (Direct)

```rust
impl RuntimeOperations for StreamRuntime {
    fn add_processor(&self, spec: ProcessorSpec) -> Result<ProcessorUniqueId> {
        // Direct implementation - already exists
        self.add_processor_impl(spec)
    }

    fn connect(&self, from: OutputLinkPortRef, to: InputLinkPortRef) -> Result<LinkUniqueId> {
        self.connect_impl(from, to)
    }

    // ... etc
}
```

### Implementation: RuntimeProxy (Channel-Based)

```rust
impl RuntimeOperations for RuntimeProxy {
    fn add_processor(&self, spec: ProcessorSpec) -> Result<ProcessorUniqueId> {
        // Send command via channel, block for response
        let (response_tx, response_rx) = oneshot::channel();

        self.command_tx
            .blocking_send((RuntimeCommand::AddProcessor { spec }, response_tx))
            .map_err(|_| StreamError::Runtime("Channel closed".into()))?;

        match response_rx.blocking_recv() {
            Ok(CommandResult::ProcessorAdded(id)) => Ok(id),
            Ok(CommandResult::Error(e)) => Err(e),
            _ => Err(StreamError::Runtime("Unexpected response".into())),
        }
    }

    // ... etc
}
```

> **💡 Note**: `RuntimeProxy` implements the sync trait using `blocking_send`/`blocking_recv`. For async contexts, use `RuntimeProxyAsync` (which has async versions of the same methods). Both share the same channel infrastructure.

### RuntimeContext Provides Unified Access

```rust
impl RuntimeContext {
    /// Get a reference to runtime operations.
    ///
    /// Works identically whether called from:
    /// - A processor's setup()/process() method
    /// - External code with runtime access
    /// - Code running via the proxy
    ///
    /// The caller doesn't need to know the underlying implementation.
    pub fn runtime(&self) -> &dyn RuntimeOperations {
        &self.runtime_ops
    }
}
```

### Usage Examples

**In a processor:**
```rust
impl Processor for MyOrchestrator {
    fn process(&mut self, ctx: &RuntimeContext) -> Result<()> {
        // Identical API - don't know/care if direct or proxied
        let camera_id = ctx.runtime().add_processor(camera_spec)?;
        let display_id = ctx.runtime().add_processor(display_spec)?;
        ctx.runtime().connect(
            OutputLinkPortRef::new(&camera_id, "video"),
            InputLinkPortRef::new(&display_id, "video"),
        )?;
        Ok(())
    }
}
```

**In external code:**
```rust
fn main() -> Result<()> {
    let runtime = StreamRuntime::new()?;

    // Same API as inside a processor
    let camera_id = runtime.add_processor(camera_spec)?;
    let display_id = runtime.add_processor(display_spec)?;
    runtime.connect(
        OutputLinkPortRef::new(&camera_id, "video"),
        InputLinkPortRef::new(&display_id, "video"),
    )?;

    runtime.start()?;
    runtime.wait_for_signal()
}
```

**In an async handler (via RuntimeProxyAsync):**
```rust
async fn create_processor(
    State(proxy): State<RuntimeProxyAsync>,
    Json(request): Json<CreateProcessorRequest>,
) -> Result<Json<CreateProcessorResponse>, AppError> {
    // Async version of the same API
    let processor_id = proxy.add_processor(spec).await?;
    Ok(Json(CreateProcessorResponse { processor_id }))
}
```

### Why This Pattern Matters

1. **Transparency**: Callers don't need to know if they're direct or proxied
2. **Testability**: Mock implementations can implement the same trait
3. **Flexibility**: Can swap implementations without changing caller code
4. **No special cases**: Same `add_processor()` works everywhere - no `_fire_and_forget` variants

### Deadlock Safety (Enabled by Event-Driven Commit)

This pattern is only safe because of the event-driven commit system (Prerequisite 4):

- Graph operations return immediately (no inline compilation)
- Commit happens asynchronously via `GraphChangeListener`
- No lock is held across the commit boundary
- Processors can safely call `ctx.runtime().add_processor()` during `process()`

Without event-driven commit, calling graph operations from inside a processor would deadlock.

---

## Lab 1: Understanding the Problem

> **📖 The Story So Far**: You have a StreamRuntime that manages processors and connections. It works great from Rust code. But now you want a web UI to control it. The web server (Axum) speaks async. The runtime speaks sync. How do you bridge them?

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
- Starves the Tokio runtime if done frequently

> **📝 Note**: With the event-driven commit system (Prerequisite 4), graph operations themselves are now fast (no inline compilation). However, calling sync functions from async context is still an anti-pattern - it blocks Tokio worker threads and defeats the purpose of async. The channel-based proxy is the correct solution.

### Why Not Make add_processor() Async?

StreamRuntime is intentionally **synchronous**:
- Apple frameworks require main thread execution
- GPU operations have specific threading requirements
- Simpler mental model for processor authors

### The Solution: Channel-Based Proxy

> **💡 Key Insight**: Instead of calling runtime methods directly from async code, we'll send *messages* through a channel. The async side sends a command and waits. The sync side polls for commands and sends back results. They never block each other.

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

> **📖 Building the Bridge**: Channels are Rust's way of sending data between threads safely. You'll use three types, each with a specific purpose. Think of them as different kinds of pipes.

### Types of Channels

| Channel Type | Use Case | Crate |
|--------------|----------|-------|
| **mpsc** | Multiple producers, single consumer | `tokio::sync::mpsc` |
| **oneshot** | Single value, one-time response | `tokio::sync::oneshot` |
| **broadcast** | Multiple consumers, all receive | `tokio::sync::broadcast` |

### Why tokio::sync::mpsc for Commands?

```rust
use tokio::sync::mpsc;

// Create bounded channel
let (tx, rx) = mpsc::channel::<RuntimeCommand>(256);

// Async send (from Tokio)
tx.send(command).await?;

// Sync receive (from main thread) - requires Mutex since try_recv takes &mut self
let mut rx = rx.lock();
while let Ok(command) = rx.try_recv() {
    // process command
}
```

We use `tokio::sync::mpsc` because:
- Already have Tokio as a dependency for the HTTP server
- Simpler than adding another crate
- `try_recv()` works without a Tokio runtime (just needs `&mut self`)
- Wrap receiver in `Mutex` for interior mutability

> **⚠️ Watch Out**: `try_recv()` takes `&mut self`, not `&self`. That's why we wrap the receiver in a `Mutex` - so we can get `&mut` access from `&self` methods. This is a common pattern when bridging sync and async code.

### Request-Response Pattern

> **💡 Pattern**: For commands that need responses, we bundle a `oneshot::Sender` with each command. The receiver uses it to send back exactly one response. Think of it like a callback, but type-safe and channel-based.

```rust
// Sender side (async)
let (response_tx, response_rx) = tokio::sync::oneshot::channel();
command_tx.send((command, response_tx)).await?;
let result = response_rx.await?;  // Wait for response

// Receiver side (sync)
let mut rx = command_rx.lock();
while let Ok((command, response_tx)) = rx.try_recv() {
    let result = execute_command(command);
    response_tx.send(result).ok();  // Send response back
}
```

---

## Lab 3: RuntimeProxy Design

> **📖 The Facade**: RuntimeProxy is the async-friendly face of StreamRuntime. It looks like the runtime (same methods), but internally it just sends messages through channels. Zero business logic - it's purely a communication facade.

### Command and Result Types

> **🎨 Design Choice**: We use enums for commands and results. Each variant maps to a runtime operation. This makes the protocol explicit and type-safe. The compiler ensures you handle every case.

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

> **💡 Type Alias Trick**: `CommandMessage` is a type alias for the tuple. This avoids repeating the complex type everywhere and sidesteps lifetime issues that would arise if we tried to embed references.

```rust
use tokio::sync::{mpsc, oneshot, broadcast};

/// Message type for command channel.
/// Using a type alias keeps things clean and avoids lifetime complexity.
pub type CommandMessage = (RuntimeCommand, oneshot::Sender<CommandResult>);

/// Async facade for StreamRuntime operations.
///
/// Obtained from `RuntimeContext::runtime_proxy()` in processor setup.
/// All methods are async and safe to call from Tokio.
#[derive(Clone)]
pub struct RuntimeProxy {
    command_tx: mpsc::Sender<CommandMessage>,
    event_tx: broadcast::Sender<RuntimeEvent>,
}

impl RuntimeProxy {
    /// Add a processor to the runtime.
    pub async fn add_processor(&self, spec: ProcessorSpec) -> Result<ProcessorUniqueId> {
        let (response_tx, response_rx) = oneshot::channel();

        self.command_tx
            .send((RuntimeCommand::AddProcessor { spec }, response_tx))
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

> **📖 Wiring It Up**: Now we add the channel infrastructure to StreamRuntime itself. The runtime owns the receiver, polls it for commands, and executes them. The proxy (which holds the sender) gets cloned to processors that need it.

### Adding Command Channel to Runtime

> **🎨 Design Note**: The channel is created in `new()`, so it exists for the lifetime of the runtime. The proxy is created at the same time, holding the sender side. Processors get clones of the proxy through `RuntimeContext`.

```rust
use tokio::sync::{mpsc, broadcast};
use parking_lot::Mutex;

pub struct StreamRuntime {
    pub(crate) compiler: Compiler,
    pub(crate) runtime_context: Mutex<Option<Arc<RuntimeContext>>>,
    pub(crate) status: Mutex<RuntimeStatus>,

    // NEW: Command channel for RuntimeProxy
    // Mutex wraps receiver since try_recv() requires &mut self
    command_rx: Mutex<mpsc::Receiver<CommandMessage>>,
    runtime_proxy: RuntimeProxy,  // Cloneable, given to processors
}

impl StreamRuntime {
    pub fn new() -> Result<Self> {
        // ... existing initialization ...

        // Create command channel (bounded)
        let (command_tx, command_rx) = mpsc::channel::<CommandMessage>(256);
        let (event_tx, _) = broadcast::channel(256);

        let runtime_proxy = RuntimeProxy { command_tx, event_tx };

        Ok(Self {
            compiler: Compiler::new(),
            runtime_context: Mutex::new(None),
            status: Mutex::new(RuntimeStatus::Initial),
            command_rx: Mutex::new(command_rx),
            runtime_proxy,
        })
    }
}
```

### Polling Commands

> **⚠️ Integration Point**: `poll_commands()` must be called regularly - either from your main loop, or integrated with the platform's event loop. On macOS, this happens in the NSApplication run loop callback.

> **✅ Safe by Design**: Thanks to the event-driven commit system (Prerequisite 4), `execute_command()` is guaranteed fast. Graph operations like `add_processor()` return immediately - they just modify data structures and publish events. Compilation happens asynchronously on the runtime thread. This means `poll_commands()` never blocks on compilation, even when processing many commands rapidly.

The runtime polls for commands in its event loop:

```rust
impl StreamRuntime {
    /// Poll and execute pending commands.
    /// Call this from your main loop or integrate with platform event loop.
    pub fn poll_commands(&self) -> usize {
        let mut count = 0;
        let mut rx = self.command_rx.lock();

        while let Ok((command, response_tx)) = rx.try_recv() {
            let result = self.execute_command(command);
            let _ = response_tx.send(result);
            count += 1;
        }

        count
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

> **📖 The Threading Dance**: This is where macOS constraints meet async Rust. The main thread belongs to Apple frameworks (NSApplication, Metal, AVFoundation). Tokio needs its own thread pool. Solution: spawn Tokio on a background thread, let it do its async thing, and communicate with the main thread via channels.

### Why Isolate Tokio?

StreamRuntime runs on the main thread (required for macOS). Tokio wants to run its own thread pool. Solution: spawn Tokio on a background thread.

> **⚠️ macOS Gotcha**: If you try to run Tokio's `block_on` directly on the main thread, you'll deadlock when any processor tries to use Apple frameworks. The main thread will be blocked waiting for Tokio, and Tokio will be waiting for main thread access. Always spawn Tokio on a separate thread.

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

> **✅ Checkpoint**: After completing this lab, you should be able to run the example and hit `curl http://localhost:9000/health`. If you get "ok", the Tokio isolation is working correctly.

---

## Lab 6: Axum HTTP Server

> **📖 The Interface**: Now we build the actual HTTP API. Axum is a modern, ergonomic web framework built on Tokio. It uses extractors to pull data from requests and makes routing declarative.

### Router Setup

> **💡 Axum Pattern**: Notice how we use `.with_state(proxy)` to share the RuntimeProxy across all handlers. Each handler receives it via `State(proxy): State<RuntimeProxy>`. This is dependency injection, Axum-style.

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

> **🎨 Pattern**: Each handler is an async function that takes extractors as arguments. `State` gives you shared state, `Json` parses the request body, `Path` extracts URL parameters. The return type determines the response format.

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

> **💡 Newtype Pattern**: `AppError` wraps `StreamError` so we can implement Axum's `IntoResponse` trait. This lets us use `?` in handlers - errors automatically convert to proper HTTP responses with status codes.

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

> **📖 Real-Time Updates**: The final piece - pushing events to connected clients as things happen. When a processor is added, all WebSocket clients hear about it instantly. This uses the `broadcast` channel we set up earlier.

### Broadcast Pattern

> **💡 tokio::select!**: This macro lets us wait on multiple async operations simultaneously. Whichever completes first wins. Here we're waiting for either: (1) a runtime event to forward, or (2) a WebSocket message from the client. This is the idiomatic way to handle bidirectional async communication.

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

> **⚠️ Lagged Clients**: Notice the `RecvError::Lagged(_)` handling. If a WebSocket client is too slow to keep up with events, the broadcast channel drops old messages. We just continue rather than disconnecting - the client will catch up with newer events.

> **✅ Checkpoint**: Test with `websocat ws://localhost:9000/api/events` in one terminal, then use curl to add a processor in another. You should see the event appear in the WebSocket terminal.

---

## API Reference

> **📋 Complete API**: Here's everything the server exposes. Use this as a quick reference when building clients.

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
- [ ] `runtime_proxy.rs` - RuntimeProxy, RuntimeProxyAsync
- [ ] `runtime_operations.rs` - RuntimeOperations trait

**Runtime Integration**:
- [ ] Update `StreamRuntime` with command channel
- [ ] Add `poll_commands()` method
- [ ] Implement `RuntimeOperations` trait for `StreamRuntime`
- [ ] Implement `RuntimeOperations` trait for `RuntimeProxy`
- [ ] Update `RuntimeContext` with `runtime()` method returning `&dyn RuntimeOperations`

**HTTP Server** (feature-gated: `api-server`):
- [ ] `libs/streamlib/src/core/service/http/mod.rs` - Server setup
- [ ] `libs/streamlib/src/core/service/http/handlers.rs` - Route handlers
- [ ] `libs/streamlib/src/core/service/http/websocket.rs` - WebSocket handling

**Processor**:
- [ ] Complete `libs/streamlib/src/core/processors/api_server.rs`

### Dependencies to Add

```toml
[dependencies]
# Already in workspace - just need correct features
tokio = { version = "1", features = ["rt-multi-thread", "net", "sync"] }

# HTTP server
axum = { version = "0.8", features = ["ws"] }
tower-http = { version = "0.6", features = ["cors"] }
```

---

## Open Questions

1. **Feature flag**: Should require `--features api-server` to include HTTP dependencies?
2. **WebSocket filtering**: Should clients subscribe to specific event types?
3. **Authentication**: Add auth middleware for production?
4. **Rate limiting**: Protect against API abuse?
5. **CORS**: Configure for browser clients?

---

## 🎉 Congratulations!

If you've completed all 7 labs, you now have:

- A **channel-based bridge** between async and sync worlds
- A **RuntimeProxy** that makes the sync runtime feel async
- **Tokio isolated** on its own thread, away from macOS constraints
- A complete **REST API** for runtime control
- **Real-time WebSocket** events for live updates

The web UI you dreamed of can now talk to StreamRuntime. Add processors, wire them together, and watch events flow - all through HTTP and WebSocket.

> **🔮 What's Next?**: Consider adding authentication (JWT or API keys), rate limiting, and CORS for production use. The Open Questions above are good starting points for discussion.
