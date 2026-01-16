# Phase 4: XPC Bridge Core - Planning Document

**Date**: January 15, 2025
**Status**: Planning

---

## Phase 3 Completed ✅

**PRs Merged:** #120, #122, #123

**Deliverables:**
- Runtime registration with broker (lifecycle tracking, API endpoint, logs)
- `streamlib logs/runtimes/broker` CLI commands
- gRPC control plane for broker diagnostics

**Important distinction:** Runtime registration is for **lifecycle management** (is runtime alive, API endpoint, logs) - NOT for XPC connection setup.

---

## Goal: Get Python Host Processors Working via XPC

The `camera-python-display` example uses `PythonContinuousProcessor`, but we have **three Python host processor types** that all need XPC bridge support:

- `PythonManualProcessor` - for generators and processors with `start()` lifecycle
- `PythonReactiveProcessor` - for frame-by-frame processing
- `PythonContinuousProcessor` - for continuous loop processing

Currently blocked because:
1. No gRPC APIs for processor host/client registration
2. No Connection struct linking the two halves
3. `PythonCore` is stubbed out
4. `_subprocess_runner.py` uses old Unix socket architecture

---

## Terminology (Read This First)

This document discusses communication between a **single processor** and its **subprocess**. Do NOT confuse these terms with StreamLib's runtime system.

| Term | Definition | Example |
|------|------------|---------|
| **Host Processor** | A Rust processor that spawns and manages a subprocess. Lives inside the StreamLib runtime. There are THREE types based on execution model - see rows below. | See subtypes below |
| ↳ **Manual Host** | Host with `start()` lifecycle. May be generator (0 inputs) or processor. Must defer `start()` if bridge not ready. | `PythonManualProcessor` |
| ↳ **Reactive Host** | Host that reacts to input frames. Always has inputs. Drops frames until bridge ready. | `PythonReactiveProcessor` |
| ↳ **Continuous Host** | Host running in a loop. May be generator or processor mode. Yields/sleeps until bridge ready. | `PythonContinuousProcessor` |
| **Client Processor** | Code running inside the subprocess that connects back to the host processor. | Python code in `_subprocess_runner.py` |
| **Connection** | The broker's record linking one host processor to one client processor. | `connection_id: "abc-123"` |
| **Broker** | The launchd service that coordinates connection setup. Never touches frame data. | `com.streamlib.broker` |
| **XPC Endpoint** | A Mach port reference that allows the client processor to connect directly to the host processor. | `xpc_endpoint_t` |

**What this is NOT about:**
- StreamLib Runtime (the execution environment) - that's already working
- Runtime registration with broker (Phase 3) - that's for lifecycle/diagnostics

**What this IS about:**
- Any of the three Python host processors spawning a Python subprocess
- That subprocess (client processor) connecting back to exchange GPU frames
- The broker coordinating that specific connection
- Each processor type has different behavior before bridge ready (defer start, drop frames, or yield/sleep)

---

## ⚠️ Pre-Implementation Cleanup (Step 0)

**CRITICAL: This XPC broker approach is the ONE AND ONLY path. All old IPC code must be deleted before implementation begins.**

### Step 0.1: Files to DELETE (if they exist)

The following code used an old Unix socket approach that is **completely obsolete**. Delete these before starting Phase 4 (they may already be deleted):

| Path | What It Was | Why Delete |
|------|-------------|------------|
| `core/subprocess/ipc/` | Unix domain socket IPC | Replaced by XPC via broker |
| `core/subprocess/host_processor.rs` | Hybrid Unix+XPC approach | Clean slate for new impl |

**NOTE**: The existing XPC code in `libs/streamlib/src/apple/subprocess_rhi/` **DOES work** for XPC frame transport. What's missing is the **gRPC signaling layer** (Connection struct, state machine, broker coordination).

### Step 0.1a: Existing XPC Infrastructure That WORKS (Keep These)

| File | What It Does | Status |
|------|--------------|--------|
| `libs/streamlib-broker/src/xpc_listener.rs` | Broker XPC listener with `register_runtime`, `get_endpoint`, `register_subprocess`, `get_subprocess_endpoint` handlers | **KEEP** - adapt for Connection struct |
| `libs/streamlib/src/apple/subprocess_rhi/xpc_channel.rs` | Full XPC channel implementation with frame transport | **KEEP** - this is the frame transport layer |
| `libs/streamlib/src/apple/subprocess_rhi/xpc_broker.rs` | Client-side broker connection | **KEEP** - adapt as needed |
| `libs/streamlib/src/apple/subprocess_rhi/xpc_frame_transport.rs` | IOSurface + xpc_shmem frame transport | **KEEP** - correct implementation |
| `libs/streamlib-broker/src/block_helpers.rs` | Block literal helpers for XPC callbacks | **KEEP** - required for XPC |

**NOTE**: XPC handles ALL frame types natively:
- **GPU frames (VideoFrame)**: `IOSurfaceCreateXPCObject` - zero-copy GPU sharing
- **CPU frames (AudioFrame, DataFrame)**: `xpc_shmem_create` - Apple's built-in XPC shared memory

The existing `xpc_frame_transport.rs` is **correct** and should be **kept** - it uses XPC's native mechanisms.

### Step 0.2: Files to REDESIGN (Not Delete)

These files exist but use the old architecture. They need **complete rewrites** using the XPC broker pattern.

**`_subprocess_runner.py` - COMPLETE REWRITE Required:**

Old architecture (DELETE all of this):
- `--control-socket /tmp/streamlib-xxx-control.sock`
- `--frames-socket /tmp/streamlib-xxx-frames.sock`
- `IpcChannel` class with `socket.socket`
- JSON messages over Unix sockets

New architecture (IMPLEMENT):
- `STREAMLIB_CONNECTION_ID` env var
- `STREAMLIB_BROKER_ENDPOINT` env var (gRPC address)
- gRPC calls to broker: `ClientAlive`, `GetHostStatus`, `MarkAcked`
- XPC calls via PyO3 wheel bindings for frame transport
- Schema exchange happens Rust-to-Rust via wheel code (Python never touches schema internals)

Files to redesign:

| Path | Current State | New Design |
|------|---------------|------------|
| `_subprocess_runner.py` | Uses old Unix sockets | Must use gRPC + XPC endpoints |
| `PythonManualProcessor` | Incomplete/stubbed | Full XPC bridge implementation |
| `PythonReactiveProcessor` | Incomplete/stubbed | Full XPC bridge implementation |
| `PythonContinuousProcessor` | Incomplete/stubbed | Full XPC bridge implementation |

### Step 0.3: SERVICE NAME DISCREPANCY - MUST FIX FIRST

**CRITICAL BUG**: There is a mismatch between hardcoded and dev-mode service names:

| Location | Service Name | Problem |
|----------|--------------|---------|
| `xpc_listener.rs:31` | `com.tatolab.streamlib.runtime` | Hardcoded, doesn't match dev mode |
| `dev-setup.sh:22` | `com.tatolab.streamlib.broker.dev-${PATH_HASH}` | Dev mode with hash suffix |

**Before any Phase 4 implementation:**
1. Update `xpc_listener.rs` to read service name from environment or compute it
2. Production: `com.tatolab.streamlib.broker` (no hash)
3. Dev mode: `com.tatolab.streamlib.broker.dev-${PATH_HASH}`

### Step 0.4: Dev Mode Service Discovery - EXPLICIT ALGORITHM

The proxy scripts (`streamlib-broker`, `streamlib`) in `.streamlib/bin/` set environment variables. The runtime reads these to determine service names.

```rust
fn get_broker_xpc_service_name() -> String {
    // STREAMLIB_BROKER_XPC_SERVICE is set by proxy scripts in dev mode
    // Production builds use the fixed name
    std::env::var("STREAMLIB_BROKER_XPC_SERVICE")
        .unwrap_or_else(|_| "com.tatolab.streamlib.broker".to_string())
}
```

**Dev mode**: Proxy scripts set `STREAMLIB_BROKER_XPC_SERVICE=com.tatolab.streamlib.broker.dev-${PATH_HASH}`

**Production**: Env var not set, defaults to `com.tatolab.streamlib.broker`

**Environment variables set by dev-setup.sh:**
- `STREAMLIB_HOME` = `/path/to/repo/.streamlib`
- `STREAMLIB_BROKER_PORT` = `50052`
- `STREAMLIB_DEV_MODE` = `1`

### What We Are NOT Doing

- ❌ Unix domain sockets for control messages
- ❌ Direct process-to-process connections without broker
- ❌ Any hybrid approach mixing old and new
- ❌ Keeping old code "for reference"

### Verification Before Starting

Before writing any Phase 4 code, confirm:
1. `core/subprocess/ipc/` directory is deleted
2. `core/subprocess/host_processor.rs` file is deleted
3. No references to Unix sockets remain in subprocess code
4. `grep -r "unix" libs/streamlib/src/core/subprocess/` returns nothing

---

## XPC Frame Serialization Design (Schema-Aware)

### The Problem

Frames are NOT just GPU buffers. A `VideoFrame` contains:
- `pixel_buffer` - GPU texture (IOSurface)
- `timestamp_ns` - i64
- `frame_number` - u64
- (potentially more fields)

An `AudioFrame` contains:
- `samples` - CPU buffer (f32 array)
- `channels` - enum
- `timestamp_ns` - i64
- `frame_number` - u64
- `sample_rate` - u32

We need to serialize ANY frame with ANY schema over XPC, and deserialize it on the other side back to the same dict structure.

### Design Principle: Transparent Serialization

**The XPC serialization is an implementation detail.** Python processors:
- Continue using the dict-based API
- Don't know XPC exists
- Don't need code changes

### FieldType → XPC Type Mapping

Every schema field maps to an XPC primitive:

| FieldType | XPC Type | Rust Function | Notes |
|-----------|----------|---------------|-------|
| `Int32` | `xpc_int64` | `xpc_int64_create()` | Widened to 64-bit |
| `Int64` | `xpc_int64` | `xpc_int64_create()` | Native |
| `UInt32` | `xpc_uint64` | `xpc_uint64_create()` | Widened to 64-bit |
| `UInt64` | `xpc_uint64` | `xpc_uint64_create()` | Native |
| `Float32` | `xpc_double` | `xpc_double_create()` | Widened to 64-bit |
| `Float64` | `xpc_double` | `xpc_double_create()` | Native |
| `Bool` | `xpc_bool` | `xpc_bool_create()` | Native |
| `String` | `xpc_string` | `xpc_string_create()` | UTF-8 |
| `Bytes` | `xpc_data` | `xpc_data_create()` | Raw bytes |
| `Texture` | IOSurface XPC | `IOSurfaceCreateXPCObject()` | Zero-copy GPU |
| `Buffer` | `xpc_shmem` | `xpc_shmem_create()` | Zero-copy CPU |
| `Array(T)` | `xpc_array` | `xpc_array_create()` | Recursive |
| `Struct` | `xpc_dictionary` | `xpc_dictionary_create()` | Recursive |
| `Optional(T)` | `xpc_null` or value | Check for null | Nullable |
| `Enum` | `xpc_string` | `xpc_string_create()` | Variant name |

### Rust Side: Serialization

**On the Host Processor (sending to subprocess):**

```rust
/// Serialize a frame to XPC dictionary using its schema.
pub fn frame_to_xpc_dictionary(
    frame: &dyn FrameData,
    schema: &Schema,
) -> *mut xpc_object_t {
    let dict = xpc_dictionary_create(null(), null(), 0);

    for field in &schema.fields {
        let key = CString::new(field.name.as_str()).unwrap();
        // ... serialize each field based on type
    }

    dict
}
```

**On the Host Processor (receiving from subprocess):**

```rust
/// Deserialize XPC dictionary back to frame using schema.
pub fn xpc_dictionary_to_frame(
    dict: *mut xpc_object_t,
    schema: &Schema,
) -> Box<dyn FrameData> {
    let mut frame = DynamicFrame::new(schema.clone());

    for field in &schema.fields {
        let key = CString::new(field.name.as_str()).unwrap();
        // ... deserialize each field based on type
    }

    Box::new(frame)
}
```

### Python Side: Deserialization

**In `_subprocess_runner.py` (receiving from host):**

```python
def xpc_dict_to_frame_dict(xpc_dict: XpcDictionary, schema: Schema) -> dict:
    """Convert XPC dictionary to Python frame dict.

    This is called internally by InputPortProxy.get().
    The result is the dict that Python processors see.
    """
    frame = {}

    for field in schema.fields:
        if field.type == "Int64":
            frame[field.name] = xpc_dict.get_int64(field.name)
        # ... handle other types

    return frame
```

**In `_subprocess_runner.py` (sending to host):**

```python
def frame_dict_to_xpc_dict(frame: dict, schema: Schema) -> XpcDictionary:
    """Convert Python frame dict to XPC dictionary.

    This is called internally by OutputPortProxy.set().
    Python processors just call .set({...}) with a normal dict.
    """
    xpc_dict = XpcDictionary.create()

    for field in schema.fields:
        value = frame.get(field.name)
        # ... serialize each field based on type

    return xpc_dict
```

### Where Serialization Happens (Transparent to User Code)

```
Python Processor Code (UNCHANGED)
         │
         │  frame = ctx.input("video").get()
         │  ctx.output("video").set({...})
         │
         ▼
┌─────────────────────────────────────┐
│     InputPortProxy / OutputPortProxy │  ← Serialization happens HERE
│  (internal to _subprocess_runner.py) │
└─────────────────────────────────────┘
```

### Schema Transmission

The schema must be known on both sides.

**Schema Exchange via gRPC (JSON format):**
1. Host processor serializes port schemas to JSON (same format as `/api/registry` endpoint)
2. Schema JSON included in gRPC `AllocateConnection` response OR separate `GetSchema` RPC
3. Client receives JSON schema via gRPC
4. Subprocess wheel code (Rust via PyO3) deserializes JSON to Schema struct
5. Python sees dict-based API, never touches schema internals

**Why gRPC, not XPC for schema:**
- XPC is for frames only (plus ACK ping/pong for connection verification)
- gRPC is for all control/signaling
- JSON schema is easily serializable over gRPC

The `InputPortProxy.get()` and `OutputPortProxy.set()` methods internally call Rust wheel code that knows the schema.

**Decision: Send schema once during connection setup via gRPC.**

Schemas don't change once a processor is loaded, so sending the full schema during the initial handshake is efficient and supports ALL frame types dynamically.

⚠️ **NEVER hardcode schemas** - this would break the majority of processors that use custom schemas. The system must support ANY dynamically declared schema.

### Summary

- **Python processors**: No changes, same dict API
- **InputPortProxy.get()**: Internally calls `xpc_dict_to_frame_dict()`
- **OutputPortProxy.set()**: Internally calls `frame_dict_to_xpc_dict()`
- **Host processor**: Uses `frame_to_xpc_dictionary()` and `xpc_dictionary_to_frame()`
- **All frame types supported**: VideoFrame, AudioFrame, DataFrame, custom schemas
- **Zero-copy where possible**: IOSurface for GPU, xpc_shmem for CPU buffers

---

## Architecture: Broker as Signalling Server

### The WebRTC Analogy

Think of this like WebRTC:
- **STUN/TURN server** = Our broker (gRPC interface)
- **Peer-to-peer data channel** = Direct XPC connection between host and subprocess

The broker **never touches frame data**. It only coordinates the connection setup.

### Two Separate Communication Channels

```
┌─────────────────────────────────────────────────────────────────┐
│                         BROKER                                   │
│                   (launchd service)                              │
│                                                                  │
│  ┌─────────────────────────────────────────────────────────┐    │
│  │                    gRPC Interface                        │    │
│  │  • AllocateConnection - get a connection_id              │    │
│  │  • HostAlive / ClientAlive - confirm processes running   │    │
│  │  • GetClientStatus / GetHostStatus - poll for readiness  │    │
│  │  • MarkAcked - confirm XPC handshake complete            │    │
│  └─────────────────────────────────────────────────────────┘    │
│                                                                  │
│  ┌─────────────────────────────────────────────────────────┐    │
│  │                    XPC Interface                         │    │
│  │  • store_endpoint - host deposits xpc_endpoint_t         │    │
│  │  • get_endpoint - client retrieves xpc_endpoint_t        │    │
│  │  (Required because Mach ports can't go through gRPC)     │    │
│  └─────────────────────────────────────────────────────────┘    │
└─────────────────────────────────────────────────────────────────┘
         ▲                                          ▲
         │ gRPC                                     │ gRPC
         │                                          │
┌────────┴────────────┐                    ┌────────┴────────────┐
│  HOST PROCESSOR    │                    │ CLIENT PROCESSOR  │
│  (Rust, e.g.       │                    │ (Python code in   │
│  PythonContinuous- │◄════ XPC ════════►│  subprocess)      │
│  Processor)        │  (frames only -    │                   │
│                    │   IOSurface)       │                   │
└─────────────────────┘                    └───────────────────┘
```

### What Goes Where

| Channel | Purpose | Data |
|---------|---------|------|
| **gRPC** | Signaling & coordination | Connection IDs, state transitions, alive/ready status, ACKs |
| **XPC (to broker)** | Endpoint brokering | `xpc_endpoint_t` objects (Mach ports) |
| **XPC (direct)** | Frame transfer | `IOSurfaceRef`, timestamps, metadata - zero-copy GPU frames |

---

## Why the Broker Needs an XPC Interface (Not Just gRPC)

**Key architectural detail**: The broker has TWO interfaces because of a macOS kernel constraint.

### The Problem with `xpc_endpoint_t`

```
xpc_endpoint_t
    │
    ├── Internally contains: Mach port send right
    │
    └── Mach ports CANNOT be serialized to bytes
        └── They're kernel objects with process-relative names
```

If you try to "serialize" an XPC endpoint to a string/bytes and send via gRPC, you get garbage. The kernel tracks port rights per-process - you can't just copy the integer.

**The ONLY ways to transfer Mach ports between processes:**
1. XPC messages (`xpc_dictionary_set_value` with the endpoint)
2. Raw Mach messaging (`mach_msg` with port descriptors)
3. Inheritance (fork, which doesn't help us with Python subprocess)

### Solution: Broker as Endpoint Relay

**Two-Part XPC Endpoint Transfer (Why Both XPC AND gRPC):**

The XPC message and gRPC call are TWO PARTS of one operation:
1. **XPC message to broker**: Transfers the actual `xpc_endpoint_t` (Mach port) - kernel handles this
2. **gRPC HostXpcReady call**: Confirms "I stored my endpoint" - updates Connection state

The gRPC call does NOT contain the endpoint (can't - Mach ports don't serialize). It just confirms the XPC transfer completed:

```
Host                           Broker
 │                               │
 │ XPC: store_endpoint           │
 │ [xpc_endpoint_t inside msg]   │
 │──────────────────────────────►│ Broker stores endpoint in Connection
 │                               │
 │ gRPC: HostXpcReady            │
 │ (just says "I stored it")    │
 │──────────────────────────────►│ Broker updates host_state to XpcReady
 │                               │
```

**Broker Can Verify Endpoint Without gRPC:** The broker CAN check `connection.host_xpc_endpoint.is_some()` to know if endpoint is stored. The gRPC call is for explicit state machine transitions and debugging visibility.

The broker is a launchd XPC service. Both the host processor and client processor can connect to it via XPC. We use the broker to **relay the XPC endpoint**.

```
HOST PROCESSOR              BROKER (launchd)           CLIENT PROCESSOR
    │                           │                              │
    │ XPC: connect              │                              │
    │══════════════════════════►│                              │
    │                           │                              │
    │ create anonymous listener │                              │
    │ get endpoint              │                              │
    │                           │                              │
    │ XPC: "store endpoint for  │                              │
    │       connection ABC"     │                              │
    │ [xpc_endpoint_t inside]   │                              │
    │──────────────────────────►│                              │
    │                           │ store in memory              │
    │                           │                              │
    │ spawn subprocess with     │                              │
    │ CONNECTION_ID=ABC         │                              │
    │═══════════════════════════════════════════════════════►│
    │                           │                              │
    │                           │              XPC: connect    │
    │                           │◄═════════════════════════════│
    │                           │                              │
    │                           │ XPC: "get endpoint for ABC"  │
    │                           │◄─────────────────────────────│
    │                           │                              │
    │                           │ XPC: "here's the endpoint"   │
    │                           │ [xpc_endpoint_t inside]      │
    │                           │─────────────────────────────►│
    │                           │                              │
    │                           │    xpc_connection_create_    │
    │                           │    from_endpoint(endpoint)   │
    │                           │                              │
    │◄══════════════════════════════════════════════════════════│
    │         Direct XPC connection for frames                 │
```

### Anonymous XPC Listeners (Host Only)

Only the **host processor** creates an anonymous XPC listener. The client connects to this listener, and the resulting connection is **bidirectional** - both sides can send and receive messages at any time.

The host processor doesn't register with launchd. Instead:

```rust
// Host creates anonymous listener (not registered with launchd)
let listener = xpc_listener_create(
    NULL,  // No name - anonymous!
    NULL,  // No target queue
    XPC_LISTENER_CREATE_INACTIVE,
    handler_block
);
xpc_listener_activate(listener);

// Get transferable endpoint reference
let endpoint = xpc_listener_copy_endpoint(listener);
// This endpoint is a Mach send right that can be transferred via XPC
```

Apple's documentation explicitly describes this pattern: *"An endpoint is a reference to a listener that can be passed to other processes. The recipient can use this reference to create a connection to the listener."*

**Bidirectional Communication:** Once the client calls `xpc_connection_create_from_endpoint(endpoint)` and the connection is established, both sides can send messages using `xpc_connection_send_message()`. The host sends via the peer connection stored in its event handler; the client sends via the connection it created.

### Why This Works with 100% Confidence

1. **Anonymous listeners are designed for this** - This is the intended use case
2. **Broker is already a launchd service** - Both processes can find it by name
3. **XPC endpoint transfer is kernel-supported** - When you put `xpc_endpoint_t` in an XPC message, the kernel handles the Mach port transfer
4. **IOSurface sharing is first-class** - `xpc_dictionary_set_value` with IOSurfaceRef works because XPC knows how to transfer the underlying Mach port

---

## Broker XPC Message Handlers

The broker's XPC interface has two operations:
- `store_endpoint` - called by host processor to deposit the XPC endpoint
- `get_endpoint` - called by client processor to retrieve the XPC endpoint

```
┌─────────────────────────────────────────────────────────────────────┐
│                              BROKER                                  │
│  ┌─────────────┐                           ┌─────────────┐          │
│  │   gRPC      │                           │    XPC      │          │
│  │  Interface  │                           │  Interface  │          │
│  └──────┬──────┘                           └──────┬──────┘          │
│         │                                         │                  │
│  • AllocateConnection                      • store_endpoint         │
│  • HostAlive/ClientAlive                   • get_endpoint           │
│  • GetClientStatus/GetHostStatus                                    │
│  • MarkAcked                               Stores: Map<conn_id,     │
│                                                    xpc_endpoint_t>  │
└─────────────────────────────────────────────────────────────────────┘
         ▲              ▲                    ▲              ▲
         │gRPC          │XPC                 │XPC           │gRPC
         │              │(endpoint)          │(endpoint)    │
┌────────┴──────────────┴────────┐  ┌───────┴──────────────┴─────────┐
│       HOST PROCESSOR           │  │      CLIENT PROCESSOR           │
│  (e.g. PythonContinuous-       │  │  (Python code in subprocess)    │
│   Processor)                   │  │                                 │
│                                │  │                                 │
│  1. gRPC: AllocateConnection   │  │  4. gRPC: ClientAlive           │
│  2. Create anonymous listener  │  │  5. XPC: get_endpoint           │
│  3. XPC: store_endpoint        │  │  6. Connect to endpoint         │
│                                │  │                                 │
│  ┌──────────────────────────┐  │  │  ┌──────────────────────────┐  │
│  │  Anonymous XPC Listener  │◄═══════►│  XPC Connection          │  │
│  │  (direct frame channel)  │  │  │  │  (from endpoint)         │  │
│  └──────────────────────────┘  │  │  └──────────────────────────┘  │
└────────────────────────────────┘  └─────────────────────────────────┘
```

### When Host Stores Endpoint (XPC message)

```rust
fn handle_store_endpoint(&self, message: xpc_object_t) -> xpc_object_t {
    let connection_id = xpc_dictionary_get_string(message, "connection_id");
    let endpoint = xpc_dictionary_get_value(message, "endpoint");

    let mut connections = self.connections.write();
    let reply = xpc_dictionary_create_reply(message);

    match connections.get_mut(connection_id) {
        None => {
            xpc_dictionary_set_bool(reply, "success", false);
            xpc_dictionary_set_string(reply, "error", "connection_not_found");
        }

        Some(conn) => {
            // Store the endpoint
            xpc_retain(endpoint); // Keep it alive!
            conn.host_xpc_endpoint = Some(endpoint);
            conn.host_state = HostState::XpcReady;
            xpc_dictionary_set_bool(reply, "success", true);
        }
    }

    reply
}
```

### When Client Requests Endpoint (XPC message) - "Chill Bro" Pattern

```rust
fn handle_get_endpoint(&self, message: xpc_object_t) -> xpc_object_t {
    let connection_id = xpc_dictionary_get_string(message, "connection_id");
    let mut connections = self.connections.write();
    let reply = xpc_dictionary_create_reply(message);

    match connections.get_mut(connection_id) {
        None => {
            xpc_dictionary_set_bool(reply, "found", false);
            xpc_dictionary_set_string(reply, "error", "connection_not_found");
            xpc_dictionary_set_string(reply, "hint",
                "Check STREAMLIB_CONNECTION_ID env var");
        }

        Some(conn) => {
            // Track that client is requesting
            conn.client_endpoint_poll_count += 1;
            conn.client_last_seen = Instant::now();

            if let Some(endpoint) = &conn.host_xpc_endpoint {
                xpc_dictionary_set_bool(reply, "found", true);
                xpc_dictionary_set_value(reply, "endpoint", *endpoint);
                conn.client_state = ClientState::XpcEndpointReceived;
            } else {
                xpc_dictionary_set_bool(reply, "found", false);
                xpc_dictionary_set_string(reply, "reason", "host_endpoint_not_ready");
                conn.client_state = ClientState::WaitingForEndpoint;
            }
        }
    }

    reply
}
```

### Example Timeline: Client Polls Before Host is Ready

This shows what happens when the subprocess starts faster than the host can set up XPC:

```
Timeline:
─────────────────────────────────────────────────────────────────────────

t=0ms    Host: AllocateConnection
         → connection_id: "abc-123"
         → Connection created, both states Pending

t=10ms   Host: HostAlive (gRPC)
         → host_state: Alive
         → host_xpc_endpoint: None (not stored yet)

t=15ms   Host spawns subprocess with CONNECTION_ID=abc-123

t=20ms   Client: ClientAlive (gRPC)
         → client_state: Alive

t=25ms   Client: get_endpoint (XPC)   ← Client is fast!
         → host_xpc_endpoint is None
         → Response: {
             found: false,
             reason: "host_endpoint_not_ready",
             host_state: "Alive",
             hint: "Host is alive but hasn't stored XPC endpoint yet.",
             your_poll_count: 1,
             timeout_remaining_secs: 299
           }
         → client_state: WaitingForEndpoint

t=50ms   Host creates XPC listener, gets endpoint

t=55ms   Host: store_endpoint (XPC)
         → host_xpc_endpoint: Some(<endpoint>)
         → host_state: XpcReady

t=75ms   Client: get_endpoint (XPC)   ← Second poll
         → host_xpc_endpoint is Some!
         → Response: { found: true, endpoint: <xpc_endpoint_t> }
         → client_state: XpcEndpointReceived

t=80ms   Client creates connection from endpoint
         Direct XPC channel established!

t=85ms   Host sends ACK ping via XPC (magic bytes: 0x53 0x4C 0x50 "SLP" = StreamLib Ping)
t=90ms   Client sends ACK pong via XPC (magic bytes: 0x53 0x4C 0x41 "SLA" = StreamLib Ack)

t=95ms   Host: MarkAcked (gRPC) → host_state: Acked
t=100ms  Client: MarkAcked (gRPC) → client_state: Acked
         → ready_at: Some(now)

         CONNECTION READY - FRAMES CAN FLOW
```

### Control Messages vs Frame Messages

Both use the same XPC channel, differentiated by `msg_type` field:
- **Control messages**: Schema exchange, ACK ping/pong, teardown - sent via XPC dictionary with `msg_type: "control"`
- **Frame messages**: Video/audio/data frames - sent via XPC dictionary with `msg_type: "frame"` + IOSurface/shmem

Control messages are ONE-TIME during setup. Frame messages are continuous during operation.

### Port Multiplexing for Multiple Inputs/Outputs

Processors may have multiple input and output ports. Rather than creating separate XPC connections per port, we **multiplex all ports over the single bidirectional connection** using a `port_name` field.

**Frame Message Format (Extended):**

```javascript
// Current:
msg_type: "frame"
frame_id: u64
handle: xpc_object_t  // IOSurface or xpc_shmem

// Extended for multiple ports:
msg_type: "frame"
frame_id: u64
port_name: "video_in" | "video_out" | "audio_in" | etc.
handle: xpc_object_t
```

**Why Multiplexing is Negligible Overhead:**
1. **One extra string field** in XPC dictionary (port name)
2. **One hash lookup** on receive to route to correct port
3. **Frame data remains zero-copy** - IOSurface for GPU, xpc_shmem for CPU

The actual frame data (which is the expensive part) is still transferred via zero-copy Mach port mechanisms. The port name is just a small string in the XPC message header.

**Receiver Side:**

```rust
fn handle_frame_message(msg: xpc_object_t) {
    let port_name = xpc_dictionary_get_string(msg, "port_name");
    let frame_id = xpc_dictionary_get_uint64(msg, "frame_id");
    let handle = xpc_dictionary_get_value(msg, "handle");

    // Route to correct port handler
    match self.port_handlers.get(port_name) {
        Some(handler) => handler.receive_frame(frame_id, handle),
        None => warn!("Unknown port: {}", port_name),
    }
}
```

### ACK Exchange Protocol

The ACK is a simple bidirectional verification that XPC data can flow:
1. **Host sends ping**: Magic bytes `0x53 0x4C 0x50` ("SLP" = StreamLib Ping)
2. **Client receives ping, sends pong**: Magic bytes `0x53 0x4C 0x41` ("SLA" = StreamLib Ack)
3. **Host receives pong**: Both directions verified working
4. **Both sides call MarkAcked via gRPC**

If pong is not received within 5 seconds, the connection attempt fails.

**Key insight**: By storing `host_xpc_endpoint: Option<xpc_endpoint_t>` directly in the Connection:
1. **Single source of truth** - Everything in one place
2. **Intelligent responses** - We say exactly why and give helpful hints
3. **Debugging** - Track poll counts, timestamps, wait times
4. **No race conditions** - Both sides update same Connection under lock

---

## Connection Model

### Lifecycle Overview

```
1. Host calls broker: "Allocate a new connection"
   → Broker generates connection_id, returns it
   → Connection in state: AwaitingBoth

2. Host starts XPC listener, posts endpoint to broker
   → Host state: XpcReady

3. Host spawns subprocess with connection_id + broker endpoint in env vars

4. Subprocess contacts broker: "I'm alive for connection X"
   → Client state: Alive

5. Subprocess retrieves host's XPC endpoint from broker via XPC interface
   → Client state: XpcEndpointReceived

6. Both sides poll broker, see other's XPC endpoint, establish XPC

7. ACK exchange via XPC (ping/pong)

8. Both sides call MarkReady
   → Connection state: Ready, frames can flow

Broker monitors: If connection doesn't reach Ready within timeout → cleanup
```

### Separating Alive vs XPC Ready

A process can be **alive** (talking to broker) but not **XPC ready** (endpoint not configured).

This separation allows:
- Detecting subprocess launch failures early (never becomes Alive)
- Diagnosing XPC setup issues vs process crashes
- Both sides confirming alive before XPC exchange matters
- Better error messages ("subprocess never started" vs "XPC setup failed")

### Host State (tracked per-connection)

```rust
pub enum HostState {
    /// Connection allocated, host hasn't contacted broker yet
    Pending,

    /// Host contacted broker via gRPC (HostAlive), but no XPC endpoint yet
    Alive,

    /// Host has stored XPC endpoint in broker via XPC message
    /// Client can now retrieve it
    XpcReady,

    /// Host has confirmed XPC connection works (received pong from client)
    Acked,

    /// Host failed (timeout, crashed, etc.)
    Failed(String),
}
```

### Client State (tracked per-connection)

```rust
pub enum ClientState {
    /// Subprocess spawned, hasn't contacted broker yet
    Pending,

    /// Client contacted broker via gRPC (ClientAlive)
    Alive,

    /// Client is polling for endpoint but hasn't received it yet
    /// (host_xpc_endpoint is still None)
    WaitingForEndpoint,

    /// Client has received the XPC endpoint from broker
    XpcEndpointReceived,

    /// Client has confirmed XPC connection works (sent pong to host)
    Acked,

    /// Client failed (timeout, crashed, etc.)
    Failed(String),
}
```

### Connection Struct (Single Source of Truth)

**xpc_endpoint_t Storage - Pattern Already Exists:**

The existing code in `xpc_listener.rs` shows how to store `xpc_endpoint_t` safely:

```rust
// xpc_endpoint_t is just *mut c_void from xpc_bindgen
pub struct Connection {
    pub host_xpc_endpoint: Option<xpc_object_t>,  // Store directly
    // ...
}

// When storing:
unsafe { xpc_retain(endpoint); }
self.connections.write().insert(conn_id, endpoint);

// When dropping:
unsafe { xpc_release(endpoint); }
```

**Do NOT research xpc-sys crate** - the existing pattern works.

The Connection struct holds **everything** about a connection - gRPC state AND XPC endpoint:

```rust
pub struct Connection {
    // ─────────────────────────────────────────────────────────────────
    // IDENTITY
    // ─────────────────────────────────────────────────────────────────

    /// Unique ID generated by broker on AllocateConnection
    pub connection_id: String,

    /// Runtime that owns this connection
    pub runtime_id: String,

    /// Processor within the runtime
    pub processor_id: String,

    // ─────────────────────────────────────────────────────────────────
    // HOST SIDE (Rust processor in runtime)
    // ─────────────────────────────────────────────────────────────────

    /// Current state of the host
    pub host_state: HostState,

    /// When host first contacted broker (gRPC: HostAlive)
    pub host_alive_at: Option<Instant>,

    /// *** THE XPC ENDPOINT ***
    /// This is the critical piece - the Mach send right that allows
    /// the client to connect to the host's anonymous XPC listener.
    /// None until host sends it via XPC message to broker.
    pub host_xpc_endpoint: Option<xpc_endpoint_t>,

    /// When the endpoint was stored (for debugging/metrics)
    pub host_xpc_endpoint_stored_at: Option<Instant>,

    /// When host confirmed ACK complete
    pub host_acked_at: Option<Instant>,

    /// Last time broker heard from host (heartbeat)
    pub host_last_seen: Instant,

    // ─────────────────────────────────────────────────────────────────
    // CLIENT SIDE (Python subprocess)
    // ─────────────────────────────────────────────────────────────────

    /// Current state of the client
    pub client_state: ClientState,

    /// When client first contacted broker (gRPC: ClientAlive)
    pub client_alive_at: Option<Instant>,

    /// Number of times client has polled for endpoint
    /// Useful for debugging ("client polled 47 times before getting endpoint")
    pub client_endpoint_poll_count: u32,

    /// First time client requested the endpoint
    pub client_first_endpoint_request_at: Option<Instant>,

    /// Have we successfully delivered the endpoint to client?
    pub client_endpoint_delivered: bool,

    /// When we delivered the endpoint
    pub client_endpoint_delivered_at: Option<Instant>,

    /// When client confirmed ACK complete
    pub client_acked_at: Option<Instant>,

    /// Last time broker heard from client (heartbeat)
    pub client_last_seen: Instant,

    // ─────────────────────────────────────────────────────────────────
    // LIFECYCLE & TIMEOUTS
    // ─────────────────────────────────────────────────────────────────

    /// When connection was allocated
    pub created_at: Instant,

    /// When both sides acked - connection is READY, frames can flow
    pub ready_at: Option<Instant>,

    /// If connection failed
    pub failed_at: Option<Instant>,
    pub failure_reason: Option<String>,

    /// Timeout in seconds (default 300 = 5 min)
    pub timeout_secs: u64,
}
```

### Derived Connection State

```rust
#[derive(Clone, Debug, PartialEq)]
pub enum DerivedConnectionState {
    /// Neither side has contacted broker yet
    AwaitingBoth,

    /// Host is alive, waiting for client subprocess to start
    AwaitingClient,

    /// Client is alive, waiting for host (unusual - host should be first)
    AwaitingHost,

    /// Both alive, but host hasn't stored XPC endpoint yet
    BothAliveAwaitingHostXpc,

    /// Host stored endpoint, client is polling to retrieve it
    ClientWaitingForHostEndpoint,

    /// Client has retrieved endpoint and is connecting
    ClientConnecting,

    /// Client connected, both sides doing ACK ping/pong
    AwaitingAck,

    /// Both ACKed - frames can flow!
    Ready,

    /// Something failed
    Failed,
}

impl Connection {
    pub fn derived_state(&self) -> DerivedConnectionState {
        // Check for failures first
        if matches!(self.host_state, HostState::Failed(_))
            || matches!(self.client_state, ClientState::Failed(_)) {
            return DerivedConnectionState::Failed;
        }

        // Check for ready
        if self.ready_at.is_some() {
            return DerivedConnectionState::Ready;
        }

        // ... derive state from host_state and client_state
    }
}
```

### State Transition Diagram

```
                ┌───────────────────┐
┌──────────────►│   AwaitingBoth    │◄──────────────┐
│               │ (conn allocated)  │               │
│               └─────────┬─────────┘               │
│                         │                         │
│         Host alive      │      Client alive       │
│               ┌─────────┴─────────┐               │
│               ▼                   ▼               │
│       ┌───────────────┐   ┌───────────────┐       │
│       │AwaitingClient │   │AwaitingHost   │       │
│       │(host alive)   │   │(client alive) │       │
│       └───────┬───────┘   └───────┬───────┘       │
│               │                   │               │
│               │  Client/Host      │               │
│               │  becomes alive    │               │
│               └─────────┬─────────┘               │
│                         ▼                         │
│               ┌───────────────────┐               │
│               │BothAliveAwaitingXpc│              │
│               │  (no XPC yet)     │               │
│               └─────────┬─────────┘               │
│                         │                         │
│                         ▼                         │
│               ┌───────────────────┐               │
│               │   HostXpcReady    │               │
│               │ (client retrieves)│               │
│               └─────────┬─────────┘               │
│                         │                         │
│               Client connects to host endpoint    │
│                         │                         │
│                         ▼                         │
│               ┌───────────────────┐               │
│               │   AwaitingAck     │               │
│               │(ping/pong via XPC)│               │
│               └─────────┬─────────┘               │
│                         │                         │
│          ACK success    │     ACK failure         │
│               ┌─────────┴─────────┐               │
│               ▼                   ▼               │
│       ┌───────────────┐   ┌───────────────┐       │
│       │     Ready     │   │    Failed     │───────┘
│       │(frames flow)  │   │  (cleanup)    │
│       └───────────────┘   └───────────────┘
│                                   ▲
│      Timeout anywhere             │
└───────────────────────────────────┘
```

### Broker Timeout Monitoring

Once a connection is allocated, the broker monitors progress:

```rust
async fn monitor_stale_connections(state: BrokerState) {
    loop {
        tokio::time::sleep(Duration::from_secs(30)).await;

        let timed_out: Vec<String> = {
            let connections = state.connections.read();
            connections.iter()
                .filter(|(_, conn)| conn.is_timed_out())
                .map(|(id, _)| id.clone())
                .collect()
        };

        for conn_id in timed_out {
            state.connections.write().remove(&conn_id);
            tracing::warn!("Connection {} timed out, removed", conn_id);
        }
    }
}
```

Default timeout: **5 minutes** from allocation to Ready.

Prevents memory leaks from:
- Subprocess that never starts
- XPC setup that hangs
- Either side crashing mid-handshake

---

## TCP-Style Handshake Protocol (via gRPC)

### Full Sequence Diagram

```
HOST PROCESSOR         BROKER                CLIENT PROCESSOR
    │                    │                          │
    │                    │                          │
    │ AllocateConnection │                          │
    │ (runtime, proc_id) │                          │
    │───────────────────►│                          │
    │◄───────────────────│ Connection allocated     │
    │ connection_id      │ state: AwaitingBoth     │
    │                    │                          │
    │ [Start XPC listener]│                          │
    │                    │                          │
    │ XPC: store_endpoint│                          │
    │ (to broker)        │                          │
    │───────────────────►│                          │
    │                    │                          │
    │ gRPC: HostXpcReady │                          │
    │───────────────────►│                          │
    │◄───────────────────│ host_state: XpcReady    │
    │                    │                          │
    │ [Spawn subprocess with env vars:]              │
    │   STREAMLIB_CONNECTION_ID=<conn_id>           │
    │   STREAMLIB_BROKER_ENDPOINT=127.0.0.1:50051   │
    │ ===========================================►│ [Process starts]
    │                    │                          │
    │                    │         ClientAlive      │
    │                    │         (conn_id)        │
    │                    │◄─────────────────────────│
    │                    │ client_state: Alive      │
    │                    │─────────────────────────►│
    │                    │ host_state=xpc_ready     │
    │                    │                          │
    │                    │     XPC: get_endpoint    │
    │                    │     (from broker)        │
    │                    │◄─────────────────────────│
    │                    │                          │
    │                    │     XPC: here's endpoint │
    │                    │─────────────────────────►│
    │                    │ client connects to host  │
    │                    │ via endpoint             │
    │                    │                          │
    │◄═══════════════════╪══════════════════════════│
    │    Direct XPC connection established          │
    │    (single bidirectional connection)          │
    │                    │                          │
    │ ACK ping (via XPC) ─────────────────────────►│
    │◄──────────────────────────────────────────────│
    │ ACK pong (via XPC) │                          │
    │                    │                          │
    │ MarkAcked(host)    │     MarkAcked(client)    │
    │───────────────────►│◄─────────────────────────│
    │                    │ Both acked → Ready       │
    │                    │                          │
    │◄═══════════════════╪══════════════════════════│
    │         Frames flow via XPC                   │
    │═══════════════════►╪═════════════════════════►│
```

### Failure Scenarios

**Subprocess never starts:**
- Host polls `GetClientStatus`, keeps seeing `client_state=pending`
- After 5 min timeout, broker marks connection Failed
- Host bridge task sees Failed, cleans up, logs error

**XPC setup fails:**
- Both sides Alive, but XPC endpoint exchange fails
- State stuck at `BothAliveAwaitingHostXpc` or `ClientWaitingForHostEndpoint`
- Timeout triggers cleanup

**ACK fails:**
- Both XPC ready, but ping/pong doesn't succeed
- State stuck at `AwaitingAck`
- Timeout triggers cleanup

**Either side crashes:**
- Last heartbeat becomes stale
- Broker can optionally add heartbeat monitoring
- Or rely on timeout for cleanup

---

## Required gRPC APIs (broker.proto additions)

### Step 1: Allocate Connection (Host only)

```protobuf
// Host processor allocates a connection before spawning subprocess
rpc AllocateConnection(AllocateConnectionRequest)
    returns (AllocateConnectionResponse);

message AllocateConnectionRequest {
    string runtime_id = 1;
    string processor_id = 2;
}

message AllocateConnectionResponse {
    string connection_id = 1;  // Pass to subprocess via env var
    bool success = 2;
    string error = 3;
}
```

### Step 2a: Host Registers Alive + XPC Ready (separate steps)

**HostAlive** = Rust host processor code is running and connected to broker.

**HostXpcReady** = Host has stored its XPC endpoint in broker (via XPC message, NOT gRPC).

```protobuf
// Host reports alive (Rust code is running)
rpc HostAlive(HostAliveRequest) returns (HostAliveResponse);

message HostAliveRequest {
    string connection_id = 1;
}

message HostAliveResponse {
    bool success = 1;
    string client_state = 2;  // "pending", "alive", "xpc_ready", etc.
}

// Host confirms XPC endpoint was stored (called AFTER XPC store_endpoint)
// NOTE: The actual endpoint is transferred via XPC, not gRPC (Mach ports can't serialize)
rpc HostXpcReady(HostXpcReadyRequest) returns (HostXpcReadyResponse);

message HostXpcReadyRequest {
    string connection_id = 1;
    // NO xpc_endpoint field - endpoint already stored via XPC message
}

message HostXpcReadyResponse {
    bool success = 1;
    string client_state = 2;
}
```

### Step 2b: Client Registers Alive

**ClientAlive** = Python subprocess is running and connected to broker.

**NOTE: Client does NOT store an XPC endpoint.** The client retrieves the host's endpoint via broker XPC interface, then connects to it. The single connection is bidirectional - no separate listener needed on the client side.

```protobuf
// Subprocess reports alive (first contact with broker)
rpc ClientAlive(ClientAliveRequest) returns (ClientAliveResponse);

message ClientAliveRequest {
    string connection_id = 1;  // From env var STREAMLIB_CONNECTION_ID
}

message ClientAliveResponse {
    bool success = 1;
    string host_state = 2;  // "pending", "alive", "xpc_ready", etc.
}
```

### Step 3: Poll for Other Side (while waiting)

```protobuf
// Host polls for client status
rpc GetClientStatus(GetClientStatusRequest) returns (GetClientStatusResponse);

message GetClientStatusRequest {
    string connection_id = 1;
}

message GetClientStatusResponse {
    string client_state = 1;  // "pending", "alive", "xpc_ready", "acked", "failed"
    // NOTE: No xpc_endpoint field - XPC endpoints transferred via broker XPC interface, not gRPC
}

// Client polls for host status
rpc GetHostStatus(GetHostStatusRequest) returns (GetHostStatusResponse);

message GetHostStatusRequest {
    string connection_id = 1;
}

message GetHostStatusResponse {
    string host_state = 1;
    // NOTE: No xpc_endpoint field - XPC endpoints transferred via broker XPC interface, not gRPC
}
```

### Step 4: Mark ACK Complete

```protobuf
// Either side marks their ACK complete
rpc MarkAcked(MarkAckedRequest) returns (MarkAckedResponse);

message MarkAckedRequest {
    string connection_id = 1;
    string side = 2;  // "host" or "client"
}

message MarkAckedResponse {
    bool success = 1;
    string connection_state = 2;  // "awaiting_ack", "ready"
}
```

### Connection Info (for debugging)

```protobuf
rpc GetConnectionInfo(GetConnectionInfoRequest) returns (GetConnectionInfoResponse);

message GetConnectionInfoRequest {
    string connection_id = 1;
}

message GetConnectionInfoResponse {
    bool found = 1;
    string host_state = 2;
    string client_state = 3;
    string derived_state = 4;
    bool host_xpc_endpoint_stored = 5;  // Whether host has stored endpoint
    bool client_connected = 6;           // Whether client has connected to host
    int64 age_secs = 7;
    int64 timeout_secs = 8;
}
```

### Step 5: Close Connection (Teardown)

```protobuf
// Called by host processor on teardown to release broker resources
rpc CloseConnection(CloseConnectionRequest) returns (CloseConnectionResponse);

message CloseConnectionRequest {
    string connection_id = 1;
    string reason = 2;  // "shutdown", "error", "timeout", etc.
}

message CloseConnectionResponse {
    bool success = 1;
}
```

The host processor calls `CloseConnection` during its teardown phase. The broker:
1. Releases the stored `xpc_endpoint_t` (calls `xpc_release`)
2. Removes the Connection from its registry
3. Logs the closure for debugging

---

## State Machines Per Processor Type

**Execution Models - Already Implemented:**

The three processor types already exist with their execution traits (`ManualProcessor`, `ReactiveProcessor`, `ContinuousProcessor`). The bridge just needs a `bridge_ready: bool` flag. Each processor type already knows what to do when not ready - no new execution model design needed.

### All Three Types Must Be Implemented

Each host processor type has different behavior **before the bridge is ready**:

| Host Type | Before Bridge Ready | Key Method | Example Use Case |
|-----------|---------------------|------------|------------------|
| **Manual** | Defer `start()`, store flag | `start()` | Python generator that produces frames on demand |
| **Reactive** | Drop incoming frames (no-op) | `process(input)` | Python filter that transforms each frame |
| **Continuous** | Yield/sleep in loop | continuous loop | Python camera source running continuously |

**The setup phase is identical for all three** - only processing behavior differs.

### Setup Phase (All Types)

`setup()` returns **immediately** - bridge setup is async:

```
setup() called
    │
    ├── Spawn Python subprocess (with broker endpoint info)
    ├── Register as SubprocessHost via gRPC
    ├── Spawn async bridge task (polls for client)
    └── Return Ok immediately
        │
        └── Bridge task runs in background...
```

### Continuous Host (`PythonContinuousProcessor`)

Used by `camera-python-display` example.

```
continuous loop:
    if bridge NOT ready:
        yield/sleep (no-op iteration)
        continue

    if has_input:
        send frame via XPC

    if subprocess has output:
        receive frame via XPC
        write to output
```

### Reactive Host (`PythonReactiveProcessor`)

Used for frame-by-frame processing where each input triggers a response.

```
process(input) called:
    if bridge NOT ready:
        drop frame (no-op)  // Plug pattern - realtime, no buffering
        return

    send input frame via XPC
    receive processed frame via XPC
    write to output
```

### Manual Host (`PythonManualProcessor`)

Used for generators or processors with explicit `start()` lifecycle.

```
start() called:
    if bridge NOT ready:
        store "start_requested" flag
        return

    forward start() to subprocess

on bridge ready:
    if "start_requested" flag set:
        send deferred start() to subprocess
```

---

## Frame Gate (Plug Pattern)

This is a **realtime system** - no buffering:

```
Upstream Processor ──► Output Plug ──► Gate ──► Subprocess
                                        │
                                   Bridge ready?
                                        │
                              No ───► Drop (no-op)
                              Yes ──► XPC send
```

Frames arriving before bridge ready are silently dropped. Upstream doesn't know, doesn't care.

---

## Implementation Tasks

### Broker Side (Phase 4a)

1. **Update broker.proto** with new RPCs:
   - AllocateConnection (returns connection_id)
   - HostAlive / HostXpcReady
   - ClientAlive (no ClientXpcReady - single connection pattern)
   - GetClientStatus / GetHostStatus (polling)
   - MarkAcked
   - GetConnectionInfo (debugging)
   - CloseConnection (teardown)

2. **Update state.rs**:
   - Add `HostState` enum (Pending, Alive, XpcReady, Acked, Failed)
   - Add `ClientState` enum (same states)
   - Add `Connection` struct with both states + XPC endpoints
   - Add `DerivedState` enum and derivation logic
   - Add timeout checking (`is_timed_out()`)

3. **Update grpc_service.rs**:
   - Implement all new RPC handlers
   - State transitions with proper locking

4. **Add background task**:
   - `monitor_stale_connections()` - cleanup timed out connections
   - Run every 30s, check `is_timed_out()`

### Host Processor Side (Phase 4b)

**Bridge Task Spawning - Use tokio::spawn:**

The bridge task is lightweight async work, spawn it with `tokio::spawn`:

```rust
impl Processor for PythonContinuousProcessor {
    async fn setup(&mut self, ctx: RuntimeContext) -> Result<()> {
        // 1. Allocate connection via gRPC
        let conn_id = broker.allocate_connection(runtime_id, processor_id).await?;

        // 2. Spawn bridge task (async, non-blocking)
        let bridge_handle = tokio::spawn(async move {
            // Create XPC listener
            // Store endpoint via XPC
            // Confirm via gRPC HostXpcReady
            // Poll for client
            // ACK exchange
            // Set bridge_ready flag
        });

        // 3. Store handle for cleanup
        self.bridge_task = Some(bridge_handle);

        // 4. Spawn subprocess
        self.subprocess = Some(spawn_python_subprocess(conn_id));

        Ok(())  // Return immediately
    }
}
```

**Must implement XPC bridge for ALL THREE host processor types:**
- `PythonManualProcessor` - defer `start()` until bridge ready
- `PythonReactiveProcessor` - drop frames until bridge ready
- `PythonContinuousProcessor` - yield/sleep until bridge ready

The core bridge logic is shared; only the "before ready" behavior differs.

1. **Connection allocation in setup()** (same for all types):
   - Call `AllocateConnection` to get `connection_id`
   - Set `STREAMLIB_CONNECTION_ID` env var for subprocess
   - Set `STREAMLIB_BROKER_ENDPOINT` env var

2. **XPC listener setup**:
   - Create anonymous XPC listener
   - Call `HostXpcReady` with endpoint

3. **Spawn subprocess**:
   - Launch Python with env vars set

4. **Bridge task (async)**:
   - Poll `GetClientStatus` until client state is `XpcEndpointReceived` (client connected)
   - Wait for client to connect to host's anonymous XPC listener
   - ACK exchange: send ping, wait for pong (via XPC)
   - Call `MarkAcked(side="host")` via gRPC
   - Set `bridge_ready` flag to true

5. **Frame gate**:
   - Check `bridge_ready` before XPC sends
   - Drop frames silently if not ready

### PyO3 XPC Bindings (Phase 4b - streamlib-python)

**Required before Python subprocess can use XPC:**

1. **XpcConnection PyO3 wrapper** (`libs/streamlib-python/src/xpc_bindings.rs`):
   - `XpcConnection` class wrapping `xpc_connection_t`
   - `connect_to_endpoint(endpoint_bytes: bytes) -> XpcConnection`
   - Connection state management (connected, disconnected, error)

2. **Frame I/O methods**:
   - `send_frame(frame_dict: dict, schema: Schema)` - serialize dict → XPC, send
   - `receive_frame(schema: Schema) -> dict` - receive XPC, deserialize → dict
   - Non-blocking `try_receive_frame()` variant

3. **Schema support**:
   - `Schema` PyO3 class (or pass schema as JSON)
   - Field type mapping for serialization/deserialization

4. **Export in wheel** (`libs/streamlib-python/src/lib.rs`):
   - Add `xpc` submodule to PyO3 module
   - Ensure bindings are available when Python imports streamlib

### Client Processor Side (Phase 4c)

**Python XPC Access - Via PyO3 Wheel:**

Python does NOT directly know or use the broker's XPC service name. Instead:
1. Python calls Rust code via PyO3 wheel bindings
2. The wheel's Rust code handles all XPC operations
3. The wheel reads `STREAMLIB_BROKER_XPC_SERVICE` env var internally
4. Python just calls `wheel.connect_to_broker()`, `wheel.get_host_endpoint()`, etc.

**The Python subprocess (`_subprocess_runner.py`) must support all three execution models:**
- Manual mode: Wait for `start()` command, then begin producing/processing
- Reactive mode: Process frames as they arrive via XPC
- Continuous mode: Run processing loop, send/receive frames continuously

**PyO3 XPC Bindings Required:**

Python cannot call XPC directly - it must use Rust code via PyO3. The `streamlib-python` wheel must expose:
- `XpcConnection` class - wrapper around `xpc_connection_t`
- `connect_to_endpoint(endpoint_bytes)` - create connection from broker-provided endpoint
- `send_frame(xpc_dict)` / `receive_frame()` - frame I/O via XPC
- Frame serialization helpers that use the schema

These bindings are loaded automatically when Python imports `streamlib` (native extension in wheel).

1. **Startup** (same for all modes):
   - Read `STREAMLIB_CONNECTION_ID` from env
   - Read `STREAMLIB_BROKER_ENDPOINT` from env
   - Connect to broker via gRPC

2. **Registration & Connection**:
   - Call `ClientAlive` immediately via gRPC
   - Request host's XPC endpoint from broker via XPC interface (`get_endpoint`)
   - Use PyO3 XPC bindings to connect to host's endpoint
   - (No ClientXpcReady needed - single bidirectional connection)

3. **ACK Exchange**:
   - Wait for ACK ping from host (magic bytes: 0x53 0x4C 0x50 "SLP")
   - Send ACK pong back (magic bytes: 0x53 0x4C 0x41 "SLA")
   - Call `MarkAcked(side="client")` via gRPC

4. **Generate Python gRPC client**:
   - Use grpcio-tools to generate from broker.proto
   - Or use betterproto for cleaner generated code

---

## Python gRPC Client (for Client Processors)

Client processors written in Python need to call broker gRPC.

**Decision: Use grpcio + grpcio-tools** to generate Python stubs from broker.proto.

### Generation Command

```bash
# From workspace root
python -m grpc_tools.protoc \
  -I libs/streamlib-broker/proto \
  --python_out=libs/streamlib-python/python/streamlib/_generated \
  --grpc_python_out=libs/streamlib-python/python/streamlib/_generated \
  broker.proto
```

### Generated Files

- `libs/streamlib-python/python/streamlib/_generated/broker_pb2.py` - Message classes
- `libs/streamlib-python/python/streamlib/_generated/broker_pb2_grpc.py` - Service stubs

### Dependencies (add to pyproject.toml)

```toml
dependencies = [
    "grpcio>=1.60.0",
    "grpcio-tools>=1.60.0",  # Only needed for regeneration
]
```

The `_subprocess_runner.py` imports these generated stubs to call `ClientAlive`, `GetHostStatus`, and `MarkAcked`.

---

## Testing Strategy

1. **Unit test broker state transitions**
2. **Integration test: Rust ↔ Broker gRPC**
3. **Integration test: Python ↔ Broker gRPC**
4. **End-to-end tests for each processor type:**
   - `PythonManualProcessor` - test `start()` deferral and deferred execution
   - `PythonReactiveProcessor` - test frame drop before ready, frame flow after ready
   - `PythonContinuousProcessor` - test yield/sleep before ready, continuous flow after ready
5. **`camera-python-display` example runs** (uses Continuous)

---

## Debugging with StreamLib CLI

The `streamlib` CLI tool provides commands to query the broker and diagnose connection issues. These are essential for debugging during development.

**IMPORTANT: Dev Environment Path**

In dev mode, always use the local CLI:
```bash
./.streamlib/bin/streamlib <command>
```

Do NOT use a global `streamlib` command - it may not exist or may be outdated.

**Currently Implemented (Phase 3):**
- `broker status` - ✅ Working
- `broker runtimes` - ✅ Working
- `logs <runtime-name>` - ✅ Working
- `broker logs` - ✅ Working

**To Be Implemented (Phase 4+):**
- `broker connections` - Pending (needs Connection registry)
- `broker processors` - Pending

### Available CLI Commands

```bash
# Check if broker is running and healthy
$ ./.streamlib/bin/streamlib broker status
Broker: healthy | Version: 0.2.4 (a1b2c3d)
Uptime: 2h 34m | Runtimes: 3 | Connections: 5 ready, 2 pending

# List all registered runtimes
$ ./.streamlib/bin/streamlib broker runtimes
RUNTIME_ID          NAME              PID     REGISTERED
runtime-abc123      camera-display    12345   2m ago
runtime-def456      test-app          12346   45m ago

# List all connections (Phase 4 will add this)
$ ./.streamlib/bin/streamlib broker connections
CONNECTION   RUNTIME      PROCESSOR              STATE    AGE
conn-001     runtime-abc  python-continuous-1    ready    2m
conn-002     runtime-abc  python-reactive-1      pending  10s

# View broker logs
$ ./.streamlib/bin/streamlib broker logs
[2025-01-15 12:34:56] INFO  Runtime registered: runtime-abc123
[2025-01-15 12:34:57] DEBUG Connection allocated: conn-001

# Follow logs in real-time
$ ./.streamlib/bin/streamlib broker logs -f

# View runtime-specific logs
$ ./.streamlib/bin/streamlib logs <runtime-name>
$ ./.streamlib/bin/streamlib logs camera-display
$ ./.streamlib/bin/streamlib logs camera-display -f  # Follow mode
```

### Debugging Connection Issues

When a host processor can't connect to its client processor, use this diagnostic flow:

```bash
# 1. Is the broker running?
$ ./.streamlib/bin/streamlib broker status

# 2. Is the runtime registered?
$ ./.streamlib/bin/streamlib broker runtimes

# 3. What's the connection state? (Phase 4 - not yet implemented)
$ ./.streamlib/bin/streamlib broker connections --runtime=<runtime-id>

# 4. Check broker logs for errors
$ ./.streamlib/bin/streamlib broker logs -f --level=debug

# 5. Check runtime logs for subprocess issues
$ ./.streamlib/bin/streamlib logs <runtime-name> -f
```

### Common Diagnostic Scenarios

| Symptom | CLI Command | What to Look For |
|---------|-------------|------------------|
| "Bridge not ready" | `streamlib broker connections` | Connection stuck in `pending` or `awaiting_*` |
| "Subprocess not responding" | `streamlib broker connections` | `client_state: pending` (subprocess never contacted broker) |
| "XPC connection failed" | `streamlib broker logs -f` | XPC errors, endpoint storage failures |
| "Timeout waiting for client" | `streamlib broker connections` | Check `age` vs `timeout_secs` |
| "Runtime not found" | `streamlib broker runtimes` | Runtime not in list, may have crashed |
| "Broker not running" | `streamlib broker status` | Status shows not running |

### JSON Output for Scripting

All commands support `--json` for machine-readable output:

```bash
$ ./.streamlib/bin/streamlib broker status --json
{
  "status": "healthy",
  "version": "0.2.4",
  "uptime_secs": 9252,
  "runtime_count": 3,
  "connection_count": 5
}

$ ./.streamlib/bin/streamlib broker connections --json
[
  {
    "connection_id": "conn-001",
    "runtime_id": "runtime-abc123",
    "processor_id": "python-continuous-1",
    "host_state": "acked",
    "client_state": "acked",
    "derived_state": "ready",
    "age_secs": 120
  }
]
```

### Log File Locations

```bash
# Broker logs (dev environment)
/tmp/streamlib-broker-dev-*.log

# Runtime logs (configured per-runtime)
# Location shown in: streamlib broker runtimes
```

### Restarting the Broker After Code Changes

**IMPORTANT**: After making changes to broker code, you MUST restart the launchd service for changes to take effect.

```bash
# Bootout stops the service, launchd auto-restarts it
$ launchctl bootout gui/$(id -u) ~/Library/LaunchAgents/com.tatolab.streamlib.broker.dev-*.plist
```

**Why restart is required**: The broker runs as a launchd service. Unlike `cargo run` which rebuilds on each invocation, the launchd service keeps the old binary running until explicitly restarted.

---

## References

- [XPC Subprocess-Listener Pattern Architecture](https://www.notion.so/2e7565009d8f81478729e3ad71249954)
- [XPC Subprocess Bridge Tasks (Kanban)](https://www.notion.so/778f421769524a04b6a62a671d44313a)
