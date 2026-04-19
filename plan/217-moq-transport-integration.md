---
whoami: amos
name: MoQ Transport Integration
status: pending
description: "FROZEN — MoQ is partially implemented but stalled on the vulkan-video path. Needs a fresh research+replan pass after the color-management umbrella (#311) lands."
github_issue: 217
dependencies:
  - "down:[BLOCKED — do not start] Pipeline-wide color management (primaries, transfer, range, tone mapping)"
adapters:
  github: builtin
---

# 🛑 FROZEN — needs replan after #311 umbrella lands 🛑

This plan is **intentionally frozen**. MoQ work is partially complete
(see [Historical snapshot](#historical-snapshot)) but was stalled
because vulkan-video was not working well enough to carry the stream.
The stabilization sequence leading through the blocked color-management
umbrella at `plan/311-color-management.md` must land first.

**Any execution of this plan must start with a research agent
producing a new issue set.** The subissue table in the historical
snapshot below is **stale** — subissues were opened against an
architecture that has since shifted (RHI overhaul, vulkan-video
integration, sync2 migration, color pipeline work). Do not pick up
old subissue numbers as-is. Re-derive the scope once the blockers
clear, then announce the new plan before opening fresh GitHub
issues.

**Blocker (must be `completed` before this plan is eligible):**

- #311 umbrella — Pipeline-wide color management (itself blocked on
  #310, #294, #293). When that umbrella completes, vulkan-video is
  stable enough to carry MoQ streams and this plan can be replanned.

If any agent picks up this plan while the blocker is still `pending`
or `in_progress`, that is a mistake — close the session and surface
the misroute to the user.

@github:tatolab/streamlib#217

---

## Historical snapshot

Everything below is a historical record of the architecture decisions
and work that landed on the `feat/217-moq-transport-integration`
branch. It is **not a todo list** — treat it as context for the
replan.

### Architecture (as implemented at freeze time)

MoQ is implemented as **composable processors** rather than link-level transport. Processors declare typed ports; the runtime wires them via iceoryx2. MoQ processors sit at the edges of the graph and bridge to the network.

```
Camera → H264Encoder → MoqPublishTrack ──→ Cloudflare Relay
                                                    ↓
Display ← H264Decoder ← MoqSubscribeTrack ←────────┘
```

#### Key Design Decisions

- **Processors, not links**: `MoqPublishTrack` and `MoqSubscribeTrack` are standard processors wired via iceoryx2. No special compiler support, no link-level annotations.
- **Per-GOP subgroup grouping**: Keyframes start new MoQ subgroups. P-frames are objects within the subgroup. Eliminates subscribe connection drops (0 drops in 40s).
- **Shared sessions**: One publish + one subscribe QUIC connection per runtime, lazily initialized via `SharedMoqSessions` on `RuntimeContext`.
- **Codec-agnostic**: MoQ transports opaque MessagePack bytes. Codec processors (H264Encoder, OpusEncoder) compose upstream/downstream.

#### Crate Stack (Revised)

- **moq-transport** (v0.14.0): MoQ session, tracks, subgroups
- **web-transport-quinn**: QUIC/WebTransport for Quinn
- **quinn** (v0.11): QUIC transport

Original plan proposed moq-lite/moq-native/hang — replaced with moq-transport which provides the same functionality in a single crate.

### Completed Work (branch feat/217-moq-transport-integration)

#### Vulkan RHI Overhaul
- Removed `gpu-allocator` crate entirely
- Centralized all Vulkan allocation in `VulkanDevice` (allocate_image_memory, allocate_buffer_memory, etc.)
- DMA-BUF export on pool resources, multi-type fallback for NVIDIA driver compatibility
- 27 RHI tests covering real processor allocation patterns
- camera-display verified rendering in VRAM via nvidia-smi + MangoHud

#### H264 Encoder (Vulkan Video)
- DPB slot count fixed (1→2 for ping-pong pattern)
- P-frame reference type corrected (uses `previous_frame_was_idr`)
- Configurable H264 profile via processor config (Main default, Baseline for WHIP)
- Empty pipelined frame skip
- 2000+ frames at ~60fps, zero device lost

#### MoQ Transport
- Publish/subscribe sessions via Cloudflare relay (draft-14)
- Per-GOP subgroup grouping (0 connection drops)
- Resilient subscribe error handling (skip cancelled, retry closed)
- Keyframe detection via Encodedvideoframe deserialization
- Shared sessions (OnceCell per runtime)
- Catalog API at `/api/moq/catalog`

#### IPC / iceoryx2
- `has_port()` defensive guard — wiring doesn't overwrite macro-generated port config
- MAX_PAYLOAD_SIZE increased to 64KB
- 8MB thread stacks for processor and display render threads
- ReadNextInOrder schema for encoded video (not yet wired via codegen)

#### FFmpeg Decoder
- LOW_DELAY + FLAG2_FAST flags for live streaming
- receive_frame loop (drains all frames per send_packet)
- Monotonic PTS per packet

#### Examples
- `moq-roundtrip`: Full pipeline — camera + audio + sensor publish, subscribe + decode + display
- camera-display: Verified working with MangoHud overlay

### Subissue table (STALE — do not action)

The tables below reflect the state at freeze time. Numbers, scopes,
and priorities are all out of date because the surrounding pipeline
has since been rewritten (RHI overhaul, vulkan-video integration,
sync2, decoder stabilization). The replan pass should re-derive
this list from scratch.

#### Completed (at freeze time)

| Issue | Title | Status |
|-------|-------|--------|
| #237 | Schema codegen fix | Done — `read_mode` and `buffer_size` wired from schema YAML into macro-generated `add_port()` calls. Continuous video decode verified working. |
| #238 | QUIC keep-alive | Done — Bypassed `ClientBuilder` to construct `Client::new()` with custom `TransportConfig`. `keep_alive_interval(4s)` set on Quinn transport, well under Cloudflare's ~10-15s idle timeout. Added `NoTlsCertificateVerification` for dev TLS path. Added `rustls-native-certs` dep. |

#### Critical — Blocks Stable Video Streaming (stale)

| Issue | Title | Description |
|-------|-------|-------------|
| #242 | SPS/DPB ref frame mismatch | `VulkanVideoSession` declares `max_num_ref_frames=1` in SPS but encoder uses 2 DPB slots (ping-pong). FFmpeg discards references, making decoder fragile after any frame loss. Compounds with #238. |

#### High — Performance & Quality (stale)

| Issue | Title | Description |
|-------|-------|-------------|
| #207 | Vulkan Video decoder | Replace FFmpeg software decode with GPU hardware decode. Zero-copy pipeline: encode GPU → MoQ → decode GPU → display. |
| #239 | IPC heap allocation | Write FramePayload directly to iceoryx2 shared memory instead of stack-allocating. Removes 64KB limit, enables 6-8Mbps+ bitrates. |

#### Low — Stability & Polish (stale)

| Issue | Title | Description |
|-------|-------|-------------|
| #229 | Separate pub/sub examples | Two-binary pattern (like WHIP/WHEP) for multi-machine testing. |
| #231 | Testing and documentation | Unit tests, integration tests, cargo doc, example READMEs. |

#### Dependency Graph (at freeze time)

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

Ordering at freeze time was #238 first, then #242, then #207 / #239
in parallel. By the time this plan is unfrozen, vulkan-video will
already be carrying bitstreams end-to-end (via the #311 umbrella and
its blockers), so #207 is effectively resolved and the rest need
re-scoping against whatever the pipeline actually looks like.

### Closed Subissues (at freeze time)

#### Completed (implemented in this branch)
- #218 — Cargo workspace setup (different deps than planned)
- #219 — Session connection to MoQ relay
- #220 — Publish path
- #221 — Subscribe path
- #226 — Schema-to-Track mapping and catalog
- #227 — Subscribe-side processor
- #228 — Roundtrip example
- #230 — Schema-agnostic data example (sensor track)

#### Superseded (processor architecture replaced link-level approach)
- #222 — OutputWriter MoQ destination type
- #223 — Compiler wiring for MoQ links
- #224 — moq_fanout flag on PortDescriptor
- #225 — Link transport annotation
