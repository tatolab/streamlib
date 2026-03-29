---
whoami: amos
name: MoQ Transport Integration
status: draft
description: Implementation plan for adding MoQ as a link-level transport alongside iceoryx2
github_issue: 217
dependencies:
  - "down:@github:tatolab/streamlib#197"
adapters:
  github: builtin
---

@github:tatolab/streamlib#217

Add Media over QUIC (MoQ) as a network-transparent link-level transport alongside iceoryx2. Processors declare typed ports with JTD schemas; the compiler chooses iceoryx2 (local, zero-copy) or MoQ (cross-network, relay fan-out) based on link configuration. Processors are transport-agnostic.

### Architectural Decision

MoQ is a **link-level transport**, not a processor sink. Unlike WebRTC (which is an opinionated sink node with fixed typed ports), MoQ is codec-agnostic and data-type-agnostic — it belongs at the same layer as iceoryx2. Any link between any two processors can be backed by MoQ if the link crosses a network boundary.

**Exception**: The subscribe side needs a graph entry point — a processor that outputs received MoQ data into the graph. This is analogous to WHEP (WebRTC subscribe-side processor).

### Key Mapping

| iceoryx2 (local) | MoQ (network) |
|---|---|
| `streamlib/{dest_proc_id}` service | `namespace/track-name` |
| `schema_name` in FramePayload | `schema_name` in MoQ track/catalog |
| MessagePack (rmp-serde) | MessagePack (same bytes, zero conversion) |
| PortKey → mailbox | Track subscription → consumer |
| `/api/registry` REST | MoQ catalog track |

### Crate Stack

- **moq-lite** (v0.15): core pub/sub transport (forwards-compatible subset of moq-transport)
- **moq-native** (v0.13): QUIC/TLS/WebTransport config for native apps (Quinn backend)
- **hang** (v0.15): media layer — catalogs, containers (optional, for relay example)

---

## Phase 0: Cargo Workspace Setup

### 0.1 MSRV Bump to 1.85

moq crates require Rust 1.85. Edition stays 2021.

**Files**:
- `Cargo.toml` (workspace root) — add `rust-version = "1.85"`
- CI configs if they pin a Rust version

### 0.2 Feature Flag & Dependencies

Add `moq` feature flag and optional dependencies to `libs/streamlib/Cargo.toml`:

```toml
[features]
moq = ["dep:moq-lite", "dep:moq-native", "dep:web-transport-quinn", "dep:quinn"]

[dependencies]
moq-lite = { version = "0.15", optional = true }
moq-native = { version = "0.13", features = ["quinn"], optional = true }
web-transport-quinn = { version = "0.10", optional = true }
quinn = { version = "0.11", optional = true }
```

Verify: `cargo check --features moq` compiles. `cargo check` (default) is unaffected.

Binary size impact estimate: ~750KB–900KB (3–4% of current binary).

---

## Phase 1: MoQ Session Management

### 1.1 MoQ Session Connection

Establish a QUIC/WebTransport session to a MoQ relay. Wrap `moq-native` connection setup.

**MoQ hierarchy**:
```
Session → Broadcast → Track → Group → Frame
```

**Producer side**: `OriginProducer` → `BroadcastProducer` → `TrackProducer` → `GroupProducer` → `FrameProducer`
**Consumer side**: `OriginConsumer` → `BroadcastConsumer` → `TrackConsumer` → `GroupConsumer` → `FrameConsumer`

Connection uses ALPN `"moq-lite-03"`. Works with Cloudflare relay.

**Cloudflare relay endpoints**:
- Draft 14: `draft-14.cloudflare.mediaoverquic.com`
- Interop: `interop-relay.cloudflare.mediaoverquic.com`

**File**: NEW `libs/streamlib/src/core/streaming/moq_session.rs` (gated behind `#[cfg(feature = "moq")]`)

### 1.2 MoQ Publish Path

Publish serialized FramePayload bytes as MoQ frames. Frames are completely opaque bytes — MessagePack-serialized FramePayloads go directly into MoQ frame payloads with zero conversion.

**No timestamps in MoQ protocol** — `FramePayload.timestamp_ns` is already embedded in the serialized payload, carries through automatically.

**Mapping**:
- `FramePayload.schema_name` → MoQ Track name
- Processor output port → MoQ Track
- Serialized FramePayload bytes → MoQ Frame payload (opaque)
- GOP/keyframe boundaries → MoQ Group sequence
- One schema type per Track

**Publish flow**: Connect → Announce broadcast → Open group stream → Send frames

**File**: NEW `libs/streamlib/src/core/streaming/moq_publisher.rs`

### 1.3 MoQ Subscribe Path

Subscribe to MoQ tracks and receive frames. Deserialize using `schema_name` from the received FramePayload.

**Subscribe flow**: Connect → Announce_please → Subscribe → Receive groups/frames

**File**: NEW `libs/streamlib/src/core/streaming/moq_subscriber.rs`

---

## Phase 2: Link-Level Integration (Publish Side)

### 2.1 OutputWriter Destination Enum

Extend OutputWriter's connection model to support MoQ as an alternative destination alongside iceoryx2.

Current OutputWriter connections:
```rust
connections: Mutex<HashMap<String, Vec<(String, String, Publisher<ipc::Service, FramePayload, ()>)>>>
```

Proposed: Add a `Destination` enum:
```rust
enum LinkOutputDestination {
    LocalIceoryx2(String, String, Publisher<ipc::Service, FramePayload, ()>),
    RemoteMoQ(String, MoQTrackPublisher),
}
```

OutputWriter already loops over multiple destinations per port — MoQ is just another destination type. Serialize once via MessagePack, publish to both local and remote destinations.

**Files**:
- `libs/streamlib/src/iceoryx2/output.rs` (lines 22–27: connections type, lines 66–127: write methods)

### 2.2 Compiler Wiring for MoQ Links

During the wiring phase (phase 6 of the 8-phase compiler transaction), detect MoQ-annotated links and create MoQ publishers alongside iceoryx2 publishers.

Insertion point: `open_iceoryx2_service_op.rs` (679 lines), the wiring operation that sets up iceoryx2 services. Add MoQ wiring logic that runs in parallel when the link is annotated for MoQ fan-out.

**Files**:
- `libs/streamlib/src/core/compiler/compiler_ops/open_iceoryx2_service_op.rs`
- Possibly a new compiler op: `open_moq_service_op.rs`

---

## Phase 3: Processor Spec & Schema Bridge

### 3.1 Add `moq_fanout` to PortDescriptor

Follow the existing `is_iceoryx2: bool` pattern (descriptors.rs line 29). Add `moq_fanout: bool` with `#[serde(default)]`.

When `moq_fanout: true`, the compiler knows to set up a MoQ publisher for that port's output in addition to (or instead of) iceoryx2.

**Files**:
- `libs/streamlib/src/core/descriptors.rs` (line 29 area, alongside `is_iceoryx2`)

### 3.2 Link Transport Annotation

Add MoQ transport metadata to links. A link can specify:
- MoQ relay endpoint URL
- Broadcast namespace
- Track name (or auto-derived from schema_name)
- Auth token (future)

Per-link config, not per-processor — different outputs might go to different MoQ endpoints.

**Files**:
- `libs/streamlib/src/core/graph/edges/link.rs` (123 lines)

### 3.3 Schema → MoQ Track Mapping & Catalog

Generate MoQ catalogs from the processor registry. Same schema names, same port definitions, advertised as MoQ tracks.

- JTD schema `com.tatolab.encodedvideoframe@1.0.0` → MoQ Track `com.tatolab.encodedvideoframe@1.0.0`
- Catalog track published alongside data tracks for remote discovery
- Uses `hang` crate for catalog format (optional)

**File**: NEW `libs/streamlib/src/core/streaming/moq_catalog.rs`

---

## Phase 4: Subscribe-Side Ingestion

### 4.1 MoQ Subscribe Processor

The one exception to "MoQ is not a processor": the subscribe side needs a processor that ingests received MoQ data into the graph. Analogous to `WebRtcWhepProcessor`.

This processor:
- Connects to a MoQ relay and subscribes to specified tracks
- Receives opaque MoQ frames → deserializes as FramePayload (MessagePack)
- Outputs typed data on its output ports based on `schema_name`
- Dynamic output ports based on subscribed tracks

Config pattern follows WHEP: endpoint URL, broadcast name, track subscriptions.

**Files**:
- NEW `libs/streamlib/src/core/processors/moq_subscribe_processor.rs`
- `libs/streamlib/src/core/processors/mod.rs` (registration)

---

## Phase 5: Examples

### 5.1 MoQ Roundtrip Example

Single binary: capture → encode → MoQ publish → subscribe → decode → display. Measures full roundtrip latency with per-stage tracing. Uses Cloudflare public relay (`https://interop-relay.cloudflare.mediaoverquic.com:443`).

**Path format**: `/{BroadcastName}` (case-sensitive, no trailing slash). No auth — use unguessable broadcast names.

**File**: NEW `examples/moq-roundtrip/`

### 5.2 Separate Publish/Subscribe Example

Two binaries (like WHIP/WHEP pattern) for multi-machine testing:
- `moq-publish`: captures and publishes to relay
- `moq-subscribe`: subscribes from relay and displays

**File**: NEW `examples/moq-publish/` and `examples/moq-subscribe/`

### 5.3 Schema-Agnostic Data Example

Non-media data over MoQ to prove the generic transport capability. E.g., publish structured telemetry/sensor data using JTD schemas, subscribe and display.

**File**: NEW `examples/moq-data/`

---

## Phase 6: Testing & Documentation

### 6.1 Unit Tests

- MoQ session connection/disconnection
- FramePayload serialization roundtrip through MoQ (MessagePack bytes → MoQ frame → MessagePack bytes)
- OutputWriter with mixed iceoryx2 + MoQ destinations
- Schema → Track name mapping

### 6.2 Integration Tests

- End-to-end publish → relay → subscribe with Cloudflare relay
- Graph compilation with MoQ-annotated links
- Processor lifecycle with MoQ subscribe processor

### 6.3 Documentation

- Update `cargo doc` for new public types
- Example READMEs with run instructions
- Update feature flag documentation

---

## Subissues

| # | Phase | Title | Complexity | Blocked By |
|---|---|---|---|---|
| #218 | 0 | Cargo workspace setup — MSRV, feature flag, deps | S | — |
| #219 | 1.1 | Session connection to MoQ relay | M | #218 |
| #220 | 1.2 | Publish path — FramePayload to MoQ frames | M | #219 |
| #221 | 1.3 | Subscribe path — MoQ frames to FramePayload | M | #219 |
| #222 | 2.1 | Extend OutputWriter with MoQ destination type | L | #220 |
| #223 | 2.2 | Compiler wiring for MoQ-annotated links | L | #222, #224, #225 |
| #224 | 3.1 | Add moq_fanout flag to PortDescriptor | S | — |
| #225 | 3.2 | Link transport annotation for MoQ metadata | S | — |
| #226 | 3.3 | Schema-to-Track mapping and MoQ catalog | M | #219, #220 |
| #227 | 4 | Subscribe processor for ingesting MoQ data | XL | #221, #218 |
| #228 | 5.1 | Roundtrip example | M | #222, #227 |
| #229 | 5.2 | Separate publish/subscribe examples | M | #222, #227 |
| #230 | 5.3 | Schema-agnostic data example | S | #222, #227 |
| #231 | 6 | Testing and documentation | L | all above |

## Dependency Graph

```
#218 (Cargo setup) ─────────────────────────────────────────┐
  │                                                          │
  ├── #219 (Session) ──┬── #220 (Publish) ── #222 (OutputWriter) ──┐
  │                    │        │                                    │
  │                    │        └── #226 (Catalog)                   ├── #223 (Compiler wiring)
  │                    │                                            │
  │                    └── #221 (Subscribe) ── #227 (Sub processor) │
  │                                                │                │
  │   #224 (moq_fanout flag) ──────────────────────┼────────────────┘
  │   #225 (Link annotation) ──────────────────────┘
  │
  ├── #228 (Roundtrip example) ──── requires #222 + #227
  ├── #229 (Pub/Sub examples) ──── requires #222 + #227
  ├── #230 (Data example) ──────── requires #222 + #227
  └── #231 (Testing & docs) ────── requires all above
```

### Parallelizable Work
- #224 (moq_fanout) + #225 (link annotation) can start immediately alongside Phase 0
- #220 (publish) and #221 (subscribe) can develop in parallel after #219
- All Phase 5 examples can develop in parallel once #222 + #227 are complete

## Risk Factors

- **API instability**: moq crates are pre-1.0, breaking changes expected — pin exact versions, isolate behind feature flag
- **Single-maintainer risk**: Luke Curley is ~84% of moq-dev commits — mitigated by small codebase (forkable) and Cloudflare's separate impl
- **Protocol still evolving**: 17 drafts — moq-lite abstracts draft differences
- **Binary size uncertainty**: estimates range 750KB–3.5MB depending on LTO and dependency tree
