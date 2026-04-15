---
whoami: amos
name: MoQ Transport Integration
status: in_progress
description: "MoQ as network transport via composable processors. Core architecture done."
github_issue: 217
dependencies:
  - "down:@github:tatolab/streamlib#197"
adapters:
  github: builtin
---

@github:tatolab/streamlib#217

## Architecture (Implemented)

MoQ is implemented as **composable processors** rather than link-level transport. Processors declare typed ports; the runtime wires them via iceoryx2. MoQ processors sit at the edges of the graph and bridge to the network.

```
Camera → H264Encoder → MoqPublishTrack ──→ Cloudflare Relay
                                                    ↓
Display ← H264Decoder ← MoqSubscribeTrack ←────────┘
```

### Key Design Decisions

- **Processors, not links**: `MoqPublishTrack` and `MoqSubscribeTrack` are standard processors wired via iceoryx2. No special compiler support, no link-level annotations.
- **Per-GOP subgroup grouping**: Keyframes start new MoQ subgroups. P-frames are objects within the subgroup. Eliminates subscribe connection drops (0 drops in 40s).
- **Shared sessions**: One publish + one subscribe QUIC connection per runtime, lazily initialized via `SharedMoqSessions` on `RuntimeContext`.
- **Codec-agnostic**: MoQ transports opaque MessagePack bytes. Codec processors (H264Encoder, OpusEncoder) compose upstream/downstream.

### Crate Stack (Revised)

- **moq-transport** (v0.14.0): MoQ session, tracks, subgroups
- **web-transport-quinn**: QUIC/WebTransport for Quinn
- **quinn** (v0.11): QUIC transport

Original plan proposed moq-lite/moq-native/hang — replaced with moq-transport which provides the same functionality in a single crate.

---

## Completed Work (branch feat/217-moq-transport-integration)

### Vulkan RHI Overhaul
- Removed `gpu-allocator` crate entirely
- Centralized all Vulkan allocation in `VulkanDevice` (allocate_image_memory, allocate_buffer_memory, etc.)
- DMA-BUF export on pool resources, multi-type fallback for NVIDIA driver compatibility
- 27 RHI tests covering real processor allocation patterns
- camera-display verified rendering in VRAM via nvidia-smi + MangoHud

### H264 Encoder (Vulkan Video)
- DPB slot count fixed (1→2 for ping-pong pattern)
- P-frame reference type corrected (uses `previous_frame_was_idr`)
- Configurable H264 profile via processor config (Main default, Baseline for WHIP)
- Empty pipelined frame skip
- 2000+ frames at ~60fps, zero device lost

### MoQ Transport
- Publish/subscribe sessions via Cloudflare relay (draft-14)
- Per-GOP subgroup grouping (0 connection drops)
- Resilient subscribe error handling (skip cancelled, retry closed)
- Keyframe detection via Encodedvideoframe deserialization
- Shared sessions (OnceCell per runtime)
- Catalog API at `/api/moq/catalog`

### IPC / iceoryx2
- `has_port()` defensive guard — wiring doesn't overwrite macro-generated port config
- MAX_PAYLOAD_SIZE increased to 64KB
- 8MB thread stacks for processor and display render threads
- ReadNextInOrder schema for encoded video (not yet wired via codegen)

### FFmpeg Decoder
- LOW_DELAY + FLAG2_FAST flags for live streaming
- receive_frame loop (drains all frames per send_packet)
- Monotonic PTS per packet

### Examples
- `moq-roundtrip`: Full pipeline — camera + audio + sensor publish, subscribe + decode + display
- camera-display: Verified working with MangoHud overlay

---

## Remaining Work

### Completed

| Issue | Title | Status |
|-------|-------|--------|
| #237 | Schema codegen fix | Done — `read_mode` and `buffer_size` wired from schema YAML into macro-generated `add_port()` calls. Continuous video decode verified working. |
| #238 | QUIC keep-alive | Done — Bypassed `ClientBuilder` to construct `Client::new()` with custom `TransportConfig`. `keep_alive_interval(4s)` set on Quinn transport, well under Cloudflare's ~10-15s idle timeout. Added `NoTlsCertificateVerification` for dev TLS path. Added `rustls-native-certs` dep. |

### Critical — Blocks Stable Video Streaming

| Issue | Title | Description |
|-------|-------|-------------|
| #242 | SPS/DPB ref frame mismatch | `VulkanVideoSession` declares `max_num_ref_frames=1` in SPS but encoder uses 2 DPB slots (ping-pong). FFmpeg discards references, making decoder fragile after any frame loss. Compounds with #238. |

### High — Performance & Quality

| Issue | Title | Description |
|-------|-------|-------------|
| #207 | Vulkan Video decoder | Replace FFmpeg software decode with GPU hardware decode. Zero-copy pipeline: encode GPU → MoQ → decode GPU → display. |
| #239 | IPC heap allocation | Write FramePayload directly to iceoryx2 shared memory instead of stack-allocating. Removes 64KB limit, enables 6-8Mbps+ bitrates. |

### Low — Stability & Polish

| Issue | Title | Description |
|-------|-------|-------------|
| #229 | Separate pub/sub examples | Two-binary pattern (like WHIP/WHEP) for multi-machine testing. |
| #231 | Testing and documentation | Unit tests, integration tests, cargo doc, example READMEs. |

### Dependency Graph

```
#237 (Schema codegen) ──→ ✓ Done
                              │
#238 (QUIC keep-alive) ───────┤ ← Immediate next step
                              │
#242 (SPS/DPB ref fix) ───────┤ ← Stable video decode
                              │
#207 (Vulkan decoder) ────────┤
                              ├──→ Broadcast-ready MoQ streaming
#239 (IPC heap alloc) ────────┘     1080p30 @ 6-8Mbps, zero-copy GPU
```

#238 is the immediate next step — eliminates the 10-15s disconnect cycle. #242 makes decode robust after any remaining frame loss. #207 and #239 are independent and can be parallelized after.

---

## Closed Subissues

### Completed (implemented in this branch)
- #218 — Cargo workspace setup (different deps than planned)
- #219 — Session connection to MoQ relay
- #220 — Publish path
- #221 — Subscribe path
- #226 — Schema-to-Track mapping and catalog
- #227 — Subscribe-side processor
- #228 — Roundtrip example
- #230 — Schema-agnostic data example (sensor track)

### Superseded (processor architecture replaced link-level approach)
- #222 — OutputWriter MoQ destination type
- #223 — Compiler wiring for MoQ links
- #224 — moq_fanout flag on PortDescriptor
- #225 — Link transport annotation
