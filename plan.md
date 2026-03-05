# Next Up

## Dependency Graph

```
#150 Unify processor schema into streamlib.yaml
  │
  │  (eliminate schemas/ duplication — macro reads from
  │   streamlib.yaml by processor name, single source of truth)
  │
  ▼
#135 streamlib-python-native FFI
  │
  │  (gives Python processors direct iceoryx2 shared memory access,
  │   eliminates 6 pipe round-trips per frame)
  │
  ▼
#144 Replace custom pubsub bus with iceoryx2
  │
  │  (architectural — consolidate all IPC onto iceoryx2,
  │   enables cross-process event observability)
  │
  ▼
#143 Remaining advanced .slpkg features
  │
  │  (JTD codegen in pack, streamlib.lock, custom schemas,
  │   namespace, URL loading in runtime API)
```

## Task List

- [x] **#150** — Unify processor schema into `streamlib.yaml`. The macro argument is always a processor name — `#[streamlib::processor("com.tatolab.camera")]` — looked up in `CARGO_MANIFEST_DIR/streamlib.yaml`. No file path support. All standalone YAML files consolidated into per-crate `streamlib.yaml` files. Eliminates `schemas/` directories and makes all Rust processors consistent with Python/TypeScript (single `streamlib.yaml` source of truth). *(PR #151)*

- [x] **#135** — streamlib-python-native FFI cdylib. Copy the `streamlib-deno-native` pattern to create `streamlib-python-native`. Gives Python subprocess processors direct iceoryx2 shared memory access via FFI, eliminating 6 pipe round-trips per frame (stdin/stdout JSON → direct shared memory read/write). *(PR #155)*

- [ ] **#144** — Replace custom pubsub bus (`core/pubsub/`) with iceoryx2 Pub/Sub. See detailed implementation plan below.

---

## #144 Implementation Plan: Replace Custom PubSub with iceoryx2

### Design Decisions (Finalized)

| Decision | Choice | Rationale |
|----------|--------|-----------|
| Payload encoding | Serialized `EventPayload` (msgpack in fixed-size `#[repr(C)]`) | Reuses existing FramePayload pattern |
| Dispatch model | Full replacement (all events through iceoryx2) | Unified messaging, no dual layer |
| Wildcard support | Single `/all` aggregate service | Simple, matches current `topics::ALL` |
| Migration strategy | Big-bang (all consumers at once) | 8 consumer files, well-understood scope |
| Pre-start events | Create `Iceoryx2Node` in `new()` not `start()` | Preserves subscriber lifecycle exactly |
| Subscriber polling | Tokio task per subscriber | Matches existing async infrastructure |
| Event size | 8KB `MAX_EVENT_PAYLOAD_SIZE` | Sufficient for all current events |

### Service Topology

```
streamlib/{runtime_id}/events/runtime    # RuntimeEvent lifecycle
streamlib/{runtime_id}/events/graph      # GraphWillChange, GraphDidChange
streamlib/{runtime_id}/events/compiler   # Compiler lifecycle events
streamlib/{runtime_id}/events/input      # Keyboard, Mouse, Window
streamlib/{runtime_id}/events/processor  # ProcessorEvent (all processors)
streamlib/{runtime_id}/events/all        # Aggregate (copy of every event)
```

### Critical Lifecycle Preservation

The current event chain must be preserved exactly:

```
StreamRuntime::new()
  ├─ Iceoryx2Node::new()                 ← MOVED from start()
  ├─ PUBSUB initializes (OnceLock)        ← now has iceoryx2 node
  ├─ GraphChangeListener subscribes       ← works because PUBSUB is ready
  │
  ├─ user calls add_processor/connect → publishes GraphDidChange
  │   └─ GraphChangeListener receives → status != Started → IGNORES (unchanged)
  │
StreamRuntime::start()
  ├─ Clone iceoryx2_node into RuntimeContext  ← instead of creating new
  ├─ status = Started
  ├─ compiler.commit() directly               ← processes all queued ops
  │
  │  ═══ GraphChangeListener now ACTIVE ═══
  │
  ├─ add_processor via API → GraphDidChange → compiler.commit() on tokio
```

### Step 1: Add `EventPayload` to `streamlib-ipc-types`

**File**: `libs/streamlib-ipc-types/src/lib.rs`

Add alongside existing `FramePayload`:
- `TopicKey` — fixed-size topic name (same pattern as `PortKey`)
- `EventPayload` — `#[repr(C)]` struct with `topic_key`, `timestamp_ns`, `len`, `data: [u8; 8192]`

### Step 2: Add event service method to `Iceoryx2Node`

**File**: `libs/streamlib/src/iceoryx2/node.rs`

Add `open_or_create_event_service()` that creates `publish_subscribe::<EventPayload>()` with:
- `max_publishers(16)` (multiple components publish)
- `subscriber_max_buffer_size(64)` (events are small, buffer more)

### Step 3: Move `Iceoryx2Node` creation from `start()` to `new()`

**Files**: `libs/streamlib/src/core/runtime/runtime.rs`

- Create `Iceoryx2Node::new()` in `StreamRuntime::new()`
- Store as field on `StreamRuntime`
- In `start()`, clone the node into `RuntimeContext::new()` instead of creating fresh

### Step 4: Rewrite `pubsub/bus.rs` with iceoryx2 backend

**File**: `libs/streamlib/src/core/pubsub/bus.rs`

Replace `PubSub` internals:
- Remove: `DashMap`, `Weak` references, `rayon::spawn` dispatch
- Add: iceoryx2 publishers (one per topic service + `/all`)
- `publish(topic, &event)`:
  1. Serialize `Event` to msgpack via `rmp_serde::to_vec_named()`
  2. Determine service name from topic string
  3. `loan_uninit()` → write `EventPayload` → `send()` on topic service
  4. `loan_uninit()` → write `EventPayload` → `send()` on `/all` service
- `subscribe(topic, callback)`:
  1. Create iceoryx2 Subscriber on topic (or `/all`) service
  2. Spawn tokio task: loop { `receive()` → deserialize → `callback(event)` }
  3. Return subscription handle (dropping it cancels the task)
- Global `PUBSUB` changes from `LazyLock<PubSub>` to `OnceLock<PubSub>`, initialized in `StreamRuntime::new()` with runtime_id + Iceoryx2Node

### Step 5: Update `EventListener` trait → callback-based

**File**: `libs/streamlib/src/core/pubsub/events.rs`

The `EventListener` trait may be preserved or replaced with a closure-based API:
- Current: `trait EventListener: Send { fn on_event(&mut self, event: &Event) -> Result<()>; }`
- Keep this trait for now — existing consumers implement it
- The iceoryx2 subscriber polling task calls `listener.lock().on_event(&event)` same as before

### Step 6: Update all publisher call sites (5 files)

Replace `PUBSUB.publish(topic, &event)` with new API in:
1. `runtime/runtime.rs` (~15 calls)
2. `runtime/operations_runtime.rs` (~12 calls)
3. `compiler/compiler.rs` (~10 calls)
4. `processors/processor_instance_factory.rs` (3 calls)
5. `signals.rs` (1 call, Linux only)

If `PUBSUB` remains a global with same `publish()` signature, these may need zero changes.

### Step 7: Update all subscriber call sites (4 files)

Replace `PUBSUB.subscribe(topic, listener)` with new API in:
1. `runtime/runtime.rs` — GraphChangeListener subscription
2. `runtime/runtime.rs` — ShutdownListener in `run_until_shutdown()`
3. `processors/api_server.rs` — WebSocket `topics::ALL` subscription
4. `utils/loop_control.rs` — shutdown-aware loop

Subscribers now need a tokio handle for the polling task. `RuntimeContext` provides this.

### Step 8: Cleanup

- Remove `dashmap` and `rayon` from `libs/streamlib/Cargo.toml`
- Delete old test code in `bus.rs` that tests DashMap/rayon dispatch
- Update `tests/pubsub_integration_test.rs`

### Consumer Migration Details

| Consumer | Current Pattern | New Pattern |
|----------|----------------|-------------|
| GraphChangeListener | `PUBSUB.subscribe(RUNTIME_GLOBAL, Arc<Mutex<dyn EventListener>>)` | Same API, iceoryx2 backend. Tokio task polls `/graph` service. |
| ShutdownListener (runtime) | `PUBSUB.subscribe(RUNTIME_GLOBAL, ...)` sets AtomicBool | Same — tokio task polls `/runtime` service, sets flag |
| ShutdownListener (loop_control) | `PUBSUB.subscribe(RUNTIME_GLOBAL, ...)` sets AtomicBool | Same — tokio task polls `/runtime` service, sets flag |
| WebSocket forwarder | `PUBSUB.subscribe(topics::ALL, ...)` sends to mpsc channel | Tokio task polls `/all` service, sends to mpsc channel |
| All publishers | `PUBSUB.publish(topic, &event)` | Same call, iceoryx2 backend serializes + sends |

### Dependency Changes

| Dependency | Action | Reason |
|------------|--------|--------|
| `dashmap` | Remove | Only used in `bus.rs` |
| `rayon` | Remove | Only used in `bus.rs` |
| `rmp-serde` | Keep | Already used for FramePayload, now also for EventPayload |
| `iceoryx2` | Keep (0.8.1) | Already a dependency |

### Files Changed (Complete List)

**streamlib-ipc-types** (1 file):
- `libs/streamlib-ipc-types/src/lib.rs` — add EventPayload, TopicKey

**streamlib iceoryx2 module** (2 files):
- `libs/streamlib/src/iceoryx2/node.rs` — add event service method
- `libs/streamlib/src/iceoryx2/mod.rs` — export new types if needed

**streamlib pubsub module** (3 files):
- `libs/streamlib/src/core/pubsub/bus.rs` — full rewrite
- `libs/streamlib/src/core/pubsub/events.rs` — remove bus-specific tests
- `libs/streamlib/src/core/pubsub/mod.rs` — update exports

**streamlib runtime** (3 files):
- `libs/streamlib/src/core/runtime/runtime.rs` — move Iceoryx2Node to new()
- `libs/streamlib/src/core/runtime/graph_change_listener.rs` — may need minor update
- `libs/streamlib/src/core/runtime/operations_runtime.rs` — publish calls (may be unchanged)

**streamlib consumers** (4 files):
- `libs/streamlib/src/core/compiler/compiler.rs` — publish calls
- `libs/streamlib/src/core/processors/processor_instance_factory.rs` — publish calls
- `libs/streamlib/src/core/processors/api_server.rs` — subscriber update
- `libs/streamlib/src/core/signals.rs` — publish call (Linux)
- `libs/streamlib/src/core/utils/loop_control.rs` — subscriber update

**Build config** (1 file):
- `libs/streamlib/Cargo.toml` — remove dashmap, rayon

**Tests** (1 file):
- `libs/streamlib/tests/pubsub_integration_test.rs` — rewrite

**Total**: ~15 files modified/rewritten

- [ ] **#143 (remaining)** — Advanced `.slpkg` features not yet implemented: JTD codegen integration in `streamlib pack`, `streamlib.lock` with file checksums, custom `schemas/` section in `ProjectConfig`, `package.namespace` field, URL loading in `runtime.load_package()`. Lower priority polish — pick items as needed.

## Issues

- https://github.com/tatolab/streamlib/issues/135
- https://github.com/tatolab/streamlib/issues/143
- https://github.com/tatolab/streamlib/issues/144
- https://github.com/tatolab/streamlib/issues/150
