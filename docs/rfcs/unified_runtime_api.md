# RFC: Unified Runtime API

## Status: Implemented

---

## Overview

A single API for runtime operations that works transparently everywhere:
- External code (`examples/camera-display`) - direct calls to `runtime.add_processor()`
- Inside processors (`process()` method) - via `ctx.runtime().add_processor()`
- External async code (API server, bolt-on features) - via `runtime.create_proxy()`

The caller doesn't know or care if they're calling directly or through a proxy.

---

## Architecture

```
┌─────────────────────────────────────────────────────────────────────────┐
│                           StreamRuntime                                  │
│                                                                          │
│  ┌──────────────────┐     ┌────────────────────────────────────────┐   │
│  │   command_tx     │────►│  Command Processing Thread             │   │
│  │   (Sender)       │     │  ┌──────────────────────────────────┐  │   │
│  └──────────────────┘     │  │ while let Ok(cmd) = rx.recv() {  │  │   │
│          │                │  │   runtime.process_command(cmd);  │  │   │
│          │ clone()        │  │ }                                 │  │   │
│          ▼                │  └──────────────────────────────────┘  │   │
│  ┌──────────────────┐     └────────────────────────────────────────┘   │
│  │  RuntimeProxy    │                        │                          │
│  │  (cheap stub)    │                        │ calls                    │
│  └──────────────────┘                        ▼                          │
│          │                    ┌──────────────────────────────┐          │
│          │                    │ impl RuntimeOperations       │          │
│          │                    │   for StreamRuntime          │          │
│          │                    │ (operations_runtime.rs)      │          │
│          │                    └──────────────────────────────┘          │
└──────────┼──────────────────────────────────────────────────────────────┘
           │
           │ given to
           ▼
┌──────────────────────────────────────────────────────────────────────────┐
│                          RuntimeContext                                   │
│  ┌────────────────────────────────────┐                                  │
│  │ runtime_ops: Arc<dyn RuntimeOperations>                               │
│  │ (points to RuntimeProxy)           │                                  │
│  └────────────────────────────────────┘                                  │
│                                                                          │
│  fn runtime() -> Arc<dyn RuntimeOperations>                              │
└──────────────────────────────────────────────────────────────────────────┘
           │
           │ used by
           ▼
┌──────────────────────────────────────────────────────────────────────────┐
│                            Processor                                      │
│                                                                          │
│  fn process(&mut self, ctx: &RuntimeContext) {                           │
│      ctx.runtime().add_processor(spec)?;  // Goes through proxy          │
│  }                                                                        │
└──────────────────────────────────────────────────────────────────────────┘
```

---

## Files

| File | Purpose |
|------|---------|
| `commands.rs` | `RuntimeCommand` enum - messages sent through channel |
| `operations.rs` | `RuntimeOperations` trait - unified interface |
| `operations_runtime.rs` | `impl RuntimeOperations for StreamRuntime` - direct execution |
| `operations_runtime_proxy.rs` | `RuntimeProxy` - channel-based stub |
| `command_receiver.rs` | `CommandReceiver` trait + impl - processes commands |
| `runtime.rs` | Spawns command thread in `new()`, `create_proxy()` method |
| `runtime_context.rs` | Holds `Arc<dyn RuntimeOperations>` (proxy) |

---

## Usage

### External code (direct)

```rust
fn main() -> Result<()> {
    let runtime = StreamRuntime::new()?;  // Returns Arc<StreamRuntime>

    // Direct call - StreamRuntime implements RuntimeOperations
    let camera_id = runtime.add_processor(camera_spec)?;
    runtime.connect(...)?;

    runtime.start()?;
    runtime.wait_for_signal()
}
```

### Inside a processor (via proxy)

```rust
fn process(&mut self, ctx: &RuntimeContext) -> Result<()> {
    // Goes through RuntimeProxy -> channel -> command thread -> StreamRuntime
    let new_processor = ctx.runtime().add_processor(spec)?;
    ctx.runtime().connect(...)?;
    Ok(())
}
```

### External bolt-on code (explicit proxy)

```rust
// For external code that needs a proxy (e.g., separate async runtime)
let proxy = runtime.create_proxy();
let proxy = Arc::new(proxy);

// Use proxy in async handlers, other threads, etc.
tokio::spawn(async move {
    let id = proxy.add_processor(spec)?;
    // ...
});
```

---

## Lifecycle

1. **`StreamRuntime::new()`**:
   - Creates command channel (`command_tx`, `command_rx`)
   - Spawns command processing thread (holds `Weak<StreamRuntime>`)
   - Thread runs for lifetime of runtime

2. **`runtime.create_proxy()`**:
   - Clones `command_tx` into new `RuntimeProxy`
   - Proxies are cheap stubs (actor model)

3. **`runtime.start()`**:
   - Creates `RuntimeProxy` via `create_proxy()`
   - Creates `RuntimeContext` with proxy
   - Processors get context, use proxy to call runtime ops

4. **`runtime.stop()`**:
   - Clears `RuntimeContext` (proxy dropped)
   - Command thread keeps running
   - Fresh context/proxy created on next `start()`

5. **Runtime dropped**:
   - `command_tx` dropped
   - When all proxies also dropped, channel closes
   - Command thread exits

---

## Why This Works

1. **Proxies are cheap** - Just a cloned channel sender
2. **No deadlocks** - Processors go through channel, not direct calls
3. **Actor model** - StreamRuntime is the actor, proxies are stubs
4. **Thread-safe** - Channel handles synchronization
5. **Lifecycle-independent** - Command processor outlives individual contexts
