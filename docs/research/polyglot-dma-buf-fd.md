<!--
Copyright (c) 2025 Jonathan Fontanez
SPDX-License-Identifier: BUSL-1.1
-->

# Research: DMA-BUF FD passing for Linux polyglot escalation

Gates the next iteration of Linux polyglot GPU access (follow-up to #325,
flagged by #320 §8.Q4 in `docs/design/gpu-capability-sandbox.md`). No code
ships with this doc — the output is a decision and a sketch.

## Question

How should a Linux Python/Deno subprocess processor receive DMA-BUF file
descriptors from a host-allocated surface (and publish its own
subprocess-allocated surfaces back to the host), given that the escalate
IPC in #325 only carries opaque pool IDs over stdio JSON-RPC?

## One-line answer

It already exists on macOS. The Linux work is filling the
`#[cfg(not(target_os = "macos"))]` stubs in
`streamlib-python-native/src/lib.rs:1339-1381` and
`streamlib-deno-native/src/lib.rs:1278-1308` with a Unix-socket
`streamlib-broker` **consumer** client — `resolve_surface` (check_out +
import) and `release`, backed by
`streamlib-broker::{send,recv}_message_with_fd` over `SCM_RIGHTS`.

No new escalate ops. No new schemas. No new IPC channels. No changes to
`SubprocessBridge`. The broker is already a standalone systemd service
listening on `$STREAMLIB_BROKER_SOCKET`; host-internal `SurfaceStore` is
already a client of it; the subprocess becomes a second client.

**Intentional divergence from macOS:** the macOS shim
(`slpn_broker_acquire_surface`) creates IOSurfaces *directly* inside
`streamlib-python-native` via `IOSurfaceCreate`. That let a subprocess
allocate outside the host's RHI, bypassing invariants the host enforces
(e.g. NVIDIA DMA-BUF pool isolation per
`docs/learnings/nvidia-dma-buf-after-swapchain.md`, VMA export-pool
discipline per `vma-export-pools.md`, VkExport flag correctness). The
Linux shim does **not** repeat this — subprocess allocation always
escalates to the host, which allocates through `GpuContextFullAccess` →
RHI → `broker.check_in`, and returns a `surface_id` to the subprocess.
Subprocess native code never calls `vkAllocateMemory`, `vkGetMemoryFdKHR`,
or `VK_EXT_external_memory_dma_buf` allocation paths. See the *Safety
posture* section for the full rationale.

## Context

#325 lands polyglot escalate-on-behalf: the subprocess sends
`{op: "acquire_pixel_buffer" | "acquire_texture" | "release_handle", …}`
over stdout and the host returns an opaque string handle. That is
enough for subprocess processors that only *reference* host resources
by name. It is not enough the moment a subprocess wants to:

- Render into a host-allocated DMA-BUF texture from its own Vulkan/Wgpu
  device.
- Sample from a DMA-BUF a host processor published into a frame.
- Publish a subprocess-allocated DMA-BUF back into the host pipeline.

All three require a real kernel file descriptor in the subprocess's FD
table so its local Vulkan/Wgpu device can `VkImportMemoryFdInfoKHR`.
Anonymous pipes (`Stdio::piped()`) cannot carry ancillary data, so any
FD path needs a socket transport separate from the stdio bridge.

The good news: that socket transport has existed for a long time on
both platforms, and the polyglot SDKs already use it on macOS.

## The existing mechanism (macOS → Linux parallel)

Every layer has a macOS implementation today and either a Linux
implementation or a clearly-marked stub:

| Layer | macOS | Linux |
| --- | --- | --- |
| Broker service binary | `streamlib-broker` bin, XPC mach-service, started via launchd (`scripts/dev-setup.sh`, `streamlib-broker/src/main.rs` macOS arm) | Same bin, Unix-socket listener at `$STREAMLIB_BROKER_SOCKET`, started via systemd (`scripts/streamlib-broker.service`) |
| Broker wire ops | `register` / `lookup` / `unregister` over XPC dictionaries (`streamlib-broker/src/xpc_service.rs:225-227`) | `register` / `lookup` / `unregister` / `check_in` / `check_out` / `release` over length-prefixed JSON + `SCM_RIGHTS` (`streamlib-broker/src/unix_socket_service.rs:181-192`) |
| FD-bearing primitive | `xpc_dictionary_set_mach_send` / `copy_mach_send` (XPC-native) | `streamlib-broker::{send,recv}_message_with_fd` (`unix_socket_service.rs:220,307`) — single FD per message via `sendmsg`/`recvmsg` + `CMSG_SPACE` |
| Host-side broker client | `SurfaceStore` macOS arm (`libs/streamlib/src/core/context/surface_store.rs`) — `check_in(pb) -> surface_id`, `check_out(surface_id) -> pb` | Same `SurfaceStore`, Linux arm — identical API, Unix-socket transport |
| Service discovery env var | `STREAMLIB_XPC_SERVICE_NAME` (set in user env by `scripts/dev-setup.sh:106,168,206`; read by host in `runtime.rs:592`, by Python subprocess at `subprocess_runner.py:106`, by Deno subprocess at `subprocess_runner.ts:143`) | `STREAMLIB_BROKER_SOCKET` (set by systemd unit `scripts/streamlib-broker.service:24` and by `dev-setup.sh:108,185,222`; read by host in `runtime.rs:620`; **not yet read by either subprocess runner**) |
| Polyglot native-lib shim (Python) | `slpn_broker_connect`, `slpn_broker_resolve_surface` (check-out / import), `slpn_broker_acquire_surface` (allocate + check-in), `slpn_broker_unregister_surface` at `streamlib-python-native/src/lib.rs:964,1038,1187,1295` | **Stubbed** at `lib.rs:1339-1381` (`#[cfg(not(target_os = "macos"))]` returns null) |
| Polyglot native-lib shim (Deno) | `sldn_broker_connect`, `sldn_broker_resolve_surface`, `sldn_broker_acquire_surface` at `streamlib-deno-native/src/lib.rs:943,1009,1163` | **Stubbed** at `lib.rs:1278-1308` |
| Polyglot Python API | `NativeGpuContextLimitedAccess.resolve_surface(surface_id)` at `processor_context.py:314`; `NativeGpuContextFullAccess.acquire_surface(w,h,fmt)` at `processor_context.py:353`; `release_pool()` at 403 | Same API. Today throws `NotImplementedError` at `gpu_surface.py:95` on Linux |
| Polyglot Deno API | `NativeGpuContextLimitedAccess.resolveSurface(poolId)` at `context.ts:321`; `NativeGpuContextFullAccess.createSurface(...)` at 351 | Same API. Today stubbed |
| Subprocess import path (handle → GPU device) | `IOSurfaceLookupFromMachPort()` at `streamlib-python-native/src/lib.rs:1129-1133` / Deno-native equivalent | `VkImportMemoryFdInfoKHR` with `VK_EXTERNAL_MEMORY_HANDLE_TYPE_DMA_BUF_BIT_EXT`. Not yet wired through the native-lib stubs; same pattern as the host's `RhiPixelBuffer::from_external_handle` Linux arm |

So the research question isn't "how do we pass FDs to a polyglot
subprocess." It's "given the macOS XPC pattern in production, which
parts of the Linux port are non-obvious?" Three things are:

1. **No Linux code has run through the polyglot shim end to end yet.**
   The broker's Unix socket protocol has been exercised by the
   host-internal `SurfaceStore`, but not by a foreign-process client
   over process boundaries.
2. **DMA-BUF FD import onto a Vulkan device that did NOT allocate it**
   requires the right external-memory extensions + possibly the right
   DRM format modifier. The macOS mach-port → IOSurface path has no
   analogue of the modifier question.
3. **The Python/Deno subprocess runners don't read
   `STREAMLIB_BROKER_SOCKET` yet.** Trivial but worth calling out.

## Prior art in-tree

Key files and their roles, for the follow-up implementation issue:

- `libs/streamlib-broker/src/main.rs:141-238` — Linux broker main loop.
  Already starts the Unix-socket surface service at
  `$STREAMLIB_BROKER_SOCKET`.
- `libs/streamlib-broker/src/unix_socket_service.rs:181-377` — the
  wire protocol. Ops, length-prefixed framing, `send_message_with_fd`,
  `recv_message_with_fd`, `MSG_CTRUNC` handling. Subprocess client
  speaks this verbatim.
- `libs/streamlib/src/core/context/surface_store.rs` — host-side
  client. Linux arm is the exact reference the polyglot native-lib
  shims should mirror (same wire, same error handling).
- `libs/streamlib-python-native/src/lib.rs:964-1295` — macOS polyglot
  shim. The Linux shim lives next door under the
  `cfg(not(target_os = "macos"))` stubs, needs to expose the same
  `slpn_broker_*` symbols with identical signatures so
  `processor_context.py` needs no platform branching.
- `libs/streamlib-deno-native/src/lib.rs:943-1308` — same story for
  Deno.
- `libs/streamlib/src/core/runtime/runtime.rs:620` — host-side host
  reads `STREAMLIB_BROKER_SOCKET` and wires `SurfaceStore`. Subprocess
  runners (`subprocess_runner.py:106`, `subprocess_runner.ts:143`)
  currently read only the macOS env var — they need the Linux read
  added.
- `libs/streamlib-python/python/streamlib/gpu_surface.py:95` — the
  `NotImplementedError` Linux wall, remove once the shim lands.

## Alternatives considered (for completeness)

### Option A — Per-subprocess socketpair side-channel (rejected)

Host opens a `socketpair(AF_UNIX, SOCK_SEQPACKET)` before spawn, passes
one end as an inherited FD, adds new `acquire_*_importable` escalate
ops, routes FD-bearing responses over the side-channel keyed by
`request_id`. Rejected: duplicates what the broker already provides,
requires `pre_exec` FD inheritance, adds new schema variants, and
forces the subprocess to poll two transports.

### Option B — iceoryx2 FD transfer (rejected)

iceoryx2 0.8.1 is zero-copy shared-memory middleware. FDs are kernel-
maintained per-process handles, not shareable-memory payload; even if
iceoryx2 layered `SCM_RIGHTS` on top of its discovery socket, it would
be strictly heavier than the existing broker. Rejected as categorically
wrong abstraction.

### Option C — Extend `SubprocessBridge` with JSON-on-stdio + FD-on-socket (rejected)

Keep all JSON on stdio; only FDs flow through a new side-channel. Same
transport cost as Option A, still reinvents what the broker handles.
Rejected.

### Option D — Polyglot SDK as another broker client (recommended)

See recommendation below. This is the macOS pattern, already in
production on that platform.

## Recommendation

**Option D.** Fill the Linux stubs in `streamlib-python-native` and
`streamlib-deno-native` with a Unix-socket `streamlib-broker` client
mirroring the macOS XPC shim, backed by
`streamlib-broker::{send,recv}_message_with_fd`. FFI surface unchanged
on both SDKs. Escalate IPC unchanged. `SurfaceStore` unchanged.

Rationale:

1. **Same pattern, same API, fewer inventions.** The macOS shim
   already defines the FFI symbols (`*_broker_resolve_surface`,
   `*_broker_acquire_surface`, `*_broker_unregister_surface`) and the
   Python/Deno call sites already use them. Linux fills the stubs with
   a different transport underneath; call sites don't branch.
2. **Broker is already running.** systemd starts it; dev shells start
   it via `dev-setup.sh`. Subprocesses don't need to launch anything.
3. **No new protocol surface.** Broker's Unix-socket wire has `check_in`
   (register + FD), `check_out` (lookup → FD via `SCM_RIGHTS`), and
   `release` — exactly what the polyglot native lib needs.
4. **Escalate IPC stays pool-IDs-only.** #325 keeps its invariants.
   The subprocess goes to the broker for FDs, not to the host.
5. **Lifetime model already works.** Broker refcounts on `check_in`
   and `release`, and cleans up dead runtimes (`main.rs:202-217`).
   Subprocess exit → socket close → broker drops the client's refs
   without host involvement.
6. **Cross-device DMA-BUF import is the same code path the host
   already exercises.** `RhiPixelBuffer::from_external_handle` works
   when the importing Vulkan device is the same process or a different
   one, provided the DMA-BUF producer used `VK_EXT_external_memory_dma_buf`.

## Safety posture — why the Linux shim diverges from macOS

The macOS polyglot shim creates IOSurfaces inside
`streamlib-python-native` / `streamlib-deno-native` directly:

```rust
// streamlib-python-native/src/lib.rs — macOS
slpn_broker_acquire_surface(w, h, fmt):
    surface = IOSurfaceCreate(...)             // ← allocation inside the polyglot crate
    mach_port = IOSurfaceCreateMachPort(surface)
    xpc_register(broker, mach_port) → surface_id
    return (surface_id, handle_to_surface)
```

Two footguns this created in practice:

1. **RHI invariants don't reach subprocess allocations.** Fixes the host
   RHI applied for NVIDIA DMA-BUF behavior, VMA export-pool discipline,
   `MAX_FRAMES_IN_FLIGHT` sizing, queue-submit mutexing — none of those
   would touch a subprocess that allocates locally on its own Vulkan
   device. Each bugfix would need to be duplicated in both native libs
   and re-verified in-process.
2. **Two parallel allocation taxonomies.** Host has typed pools,
   capability-gated allocation, central tracking in `GpuContext`.
   Subprocess-local allocation has none of that — it's an ad-hoc
   Vulkan device inside a cdylib, with the same class of bugs the
   Unreal-style RHI centralization is meant to prevent (CLAUDE.md
   "Engine Model").

The Linux shim closes both by making allocation host-only:

```
# Subprocess (Python / Deno)
ctx.acquire_surface(w, h, fmt):            # stays the same API
    surface_id = escalate_rpc("acquire_pixel_buffer", ...)   # #325 flow
    fd, size, metadata = broker.check_out(surface_id)        # Linux addition
    vk_handle = vk_import_memory_fd(fd, size, ...)
    return NativeGpuSurfaceHandle(surface_id, vk_handle)
```

- **Allocation** is always `GpuContextFullAccess::acquire_pixel_buffer`
  on the host (which already `check_in`s with the broker on the Linux
  arm of `SurfaceStore`).
- **Subprocess native lib** only implements the consumer side:
  `check_out` (read message + FD), `vkImportMemoryFdKHR`, and
  `release`.
- **`VK_EXT_external_memory_dma_buf` allocation paths do not exist in
  `streamlib-*-native`.** Only the *import* extension + `vkImportMemoryFdKHR`.
- **Steady-state cost:** zero overhead vs. the macOS local-allocation
  pattern. Pools are escalate-allocated once at setup, and the per-frame
  path is either a `check_out` (if the FD isn't cached in the subprocess
  yet) or nothing (once cached). The escalate cost is amortized across
  pool depth.

Follow-up work can re-evaluate whether the macOS shim should adopt the
same posture (strip `IOSurfaceCreate` out of the native libs and route
through escalate on macOS too). Out of scope for this research —
flagged as a cleanup candidate.

## Deliverable 1 — IPC-schema changes

**None** in the escalate request/response schemas. `#325`'s
`acquire_pixel_buffer` / `acquire_texture` / `release_handle` stay
exactly as they are — they remain the host-internal control path for
resources the subprocess wants the host to allocate and track, and
their opaque handle IDs stay opaque to the subprocess.

The broker's wire format is schema-less JSON today
(`unix_socket_service.rs:181-192` uses `serde_json::Value` dispatch on
the `op` string) because it's a hand-authored internal protocol — the
Linux polyglot shim continues that pattern. If the user wants the
broker protocol to be schema-tracked under `libs/streamlib/schemas/`,
that's a larger refactor covering macOS too, and it's out of scope
here.

One minor schema-adjacent question covered in Open Questions below:
whether the broker's `check_in` response should carry DRM-format-modifier
metadata, or whether that stays out-of-band.

## Deliverable 2 — Host-side routing in `SubprocessHostProcessor`

**Nothing new.** The subprocess talks to the broker directly over the
inherited `STREAMLIB_BROKER_SOCKET`. The host is not on the FD path.

One small fix in the host-side spawners:

- `spawn_python_native_subprocess_op.rs:125-138` sets an explicit env
  whitelist for the subprocess. `STREAMLIB_BROKER_SOCKET` needs to be
  forwarded the same way `STREAMLIB_XPC_SERVICE_NAME` is today. (On
  macOS, the XPC var is inherited implicitly because the parent env
  survives the explicit `.env(...)` calls; confirm which behavior
  applies here and make it explicit to avoid surprise.)

## Deliverable 3 — Subprocess-side client surface

No Python- or Deno-visible API change. The existing user-facing methods
stay the same. What changes is what runs behind them on Linux:

**Consumer (subprocess reads a host-produced surface):**

```python
# processor_context.py — already exists, stays the same
surface = gpu.resolve_surface(surface_id)  # surface_id from frame envelope
# Linux native lib:
#   1. Lazy-connect to $STREAMLIB_BROKER_SOCKET if not already connected.
#   2. send_message_with_fd({op: "check_out", surface_id})
#   3. recv_message_with_fd → DMA-BUF FD (via SCM_RIGHTS) + metadata
#      (size, width, height, format)
#   4. vkImportMemoryFdKHR onto subprocess-owned Vulkan device
#   5. Return NativeGpuSurfaceHandle pointing at imported VkImage.
#   6. Cache: next resolve_surface(same_id) short-circuits to the
#      cached VkImage so repeat access doesn't re-check-out.
```

**Producer (subprocess wants to publish a surface downstream):**

```python
# processor_context.py — already exists, stays the same
handle = gpu_full.acquire_surface(width, height, format)
# Linux flow:
#   1. Subprocess escalates acquire_pixel_buffer over #325's stdio IPC.
#   2. Host GpuContextFullAccess allocates the DMA-BUF (through the
#      RHI, honoring NVIDIA workarounds + VMA pool discipline).
#   3. Host SurfaceStore.check_in(pb) → surface_id.
#   4. Host returns surface_id to subprocess over stdio escalate
#      response.
#   5. Subprocess-side resolve_surface(surface_id) (as above) —
#      check_out + import into subprocess-owned Vulkan device.
#   6. handle.pool_id = surface_id. Subprocess renders via the
#      imported VkImage, then publishes surface_id in its iceoryx2
#      frame envelope.
#
# streamlib-*-native does NOT call vkAllocateMemory, vkGetMemoryFdKHR,
# or VK_EXT_external_memory_dma_buf allocation. Only import-side
# extensions.
```

**Release:**

```python
gpu.release_pool(pool_id)
# Linux native lib: send_message_with_fd({op: "release", surface_id: pool_id})
# plus subprocess-local cache eviction (drop the imported VkImage).
# Host-side release_handle (over stdio escalate) drops the host's
# registry entry and the underlying VkDeviceMemory.
```

The Python and Deno modules don't branch on `sys.platform` — the FFI
symbol does the right thing on each OS, and `acquire_surface` internally
does the escalate-then-resolve dance on Linux while doing the direct
XPC path on macOS (current behavior).

### Deno caveat

Deno's stdlib doesn't expose `recvmsg` ancillary data, but this
doesn't matter: the Linux shim is in `streamlib-deno-native` (Rust),
not in Deno userland. The `sldn_broker_*` FFI symbols do all the
`sendmsg`/`recvmsg` work in Rust and return pre-imported Vulkan
handles to Deno.

## Deliverable 4 — Platform conditionalization

```rust
// libs/streamlib-python-native/src/lib.rs

#[cfg(target_os = "macos")]
mod broker_macos { /* existing XPC shim, lines 964-1295 */ }
#[cfg(target_os = "macos")]
pub use broker_macos::*;

#[cfg(target_os = "linux")]
mod broker_linux {
    // CONSUMER-ONLY client. Wire ops: check_out, release.
    // Transport: UnixStream to $STREAMLIB_BROKER_SOCKET, lazy-connect
    //            on first use.
    // FD carrier: streamlib_broker::send_message_with_fd /
    //             streamlib_broker::recv_message_with_fd.
    // Import only: VkImportMemoryFdInfoKHR with DMA_BUF_BIT_EXT.
    // Deliberately NO: vkAllocateMemory, vkGetMemoryFdKHR, or any
    //                  VK_EXT_external_memory_dma_buf allocation path.
    //                  Allocation goes through #325 escalate →
    //                  GpuContextFullAccess → RHI → SurfaceStore.check_in.
}
#[cfg(target_os = "linux")]
pub use broker_linux::*;
```

Same shape in `streamlib-deno-native`. The module split already exists
as `#[cfg(not(target_os = "macos"))]` stubs — this replaces the stubs
with a real implementation behind `#[cfg(target_os = "linux")]`, and
deliberately does **less** than the macOS shim does (no producer
allocation path).

`SurfaceStore` is untouched. `SubprocessBridge` is untouched. `#325`'s
escalate ops are untouched — though on Linux, `acquire_pixel_buffer` /
`acquire_texture` escalate handlers gain a `SurfaceStore::check_in`
call after allocation so the broker knows about the buffer and the
subprocess's subsequent `check_out` succeeds. The files in scope are:

- `libs/streamlib-python-native/src/lib.rs` — consumer-only Linux shim
- `libs/streamlib-deno-native/src/lib.rs` — same for Deno
- `libs/streamlib-python/python/streamlib/subprocess_runner.py:106` —
  add `STREAMLIB_BROKER_SOCKET` read
- `libs/streamlib-deno/subprocess_runner.ts:143` — same
- `libs/streamlib-python/python/streamlib/gpu_surface.py:95` — drop
  the Linux `NotImplementedError`
- `libs/streamlib/src/core/compiler/compiler_ops/subprocess_escalate.rs` —
  Linux-arm `acquire_*` handlers call `SurfaceStore::check_in` after
  allocating and return the broker `surface_id`

## Decisions made in review

Resolved in discussion. Recording here so the follow-up implementation
issue inherits the context and so reviewers can challenge specific
points without re-opening the whole design.

### Allocation stays on the host (safety > matching macOS)

Subprocess polyglot native libs get the **consumer half only**:
`resolve_surface` (check_out + `vkImportMemoryFdKHR` + cache) and
`release`. No `acquire_surface`, no local Vulkan allocation. Allocation
requests go through the existing `#325` escalate path, the host
allocates via `GpuContextFullAccess`, `check_in`s with the broker, and
returns a `surface_id` to the subprocess. See the *Safety posture*
section above.

### Socket type: `SOCK_STREAM` with length-prefix framing

The broker already speaks `SOCK_STREAM` (default type for `UnixListener::bind`)
with a 4-byte big-endian length prefix. The Linux polyglot shim uses
the exact same framing via the existing
`streamlib-broker::{send,recv}_message_with_fd` helpers. `SOCK_SEQPACKET`
would be marginally nicer at the `recvmsg` call site (one syscall
instead of two, atomic message + ancillary-data delivery) but changing
the broker to support both socket types is protocol churn for
negligible realtime gain. Stay with STREAM.

### First cut: linear / implicit DRM format modifier only

`VK_EXT_image_drm_format_modifier` is deferred. The first Linux
polyglot cut allocates DMA-BUFs with the implicit / `DRM_FORMAT_MOD_INVALID`
modifier — matching the simple allocation path `VulkanPixelBuffer`
already uses for cross-process export. Broker `check_in` / `check_out`
JSON payloads do not grow a `drm_format_modifier` field yet. When a
tiled-layout producer (e.g. a hardware encoder feeding a polyglot
consumer) surfaces, a follow-up ticket adds explicit modifier
negotiation on both arms. Documented as a known constraint, not a TODO.

### Broker connection is lazy, fails at first use

Subprocess doesn't try to connect at startup. The first call to
`resolve_surface` (or any future broker-backed op) attempts the
connection; failure returns a hard error to the caller with a clear
message pointing at `sudo systemctl start streamlib-broker` or
`scripts/dev-setup.sh`. Subprocesses that never touch GPU surfaces
don't need the broker up.

Rationale: decouples subprocess lifecycle from broker lifecycle. Avoids
the "subprocess starts, broker not up yet, weird silent failures later"
devex footgun. Avoids the "fail hard at startup even though this
subprocess doesn't need surfaces" overreach.

This is a deliberate divergence from macOS's
`subprocess_runner.ts:154` behavior (log + disable). The macOS arm can
follow the Linux pattern in a cleanup pass — not required for this
research.

### Env var propagation: inherited, no new injection

The host spawner (`spawn_python_native_subprocess_op.rs:125-145`)
doesn't call `.env_clear()`, so the parent env propagates to children
automatically. `STREAMLIB_BROKER_SOCKET` set by systemd
(`scripts/streamlib-broker.service:24`) or `dev-setup.sh` reaches the
subprocess without any host-side injection change. The only new code
on the subprocess side is adding `os.environ.get("STREAMLIB_BROKER_SOCKET")`
to `subprocess_runner.py` and `Deno.env.get("STREAMLIB_BROKER_SOCKET")`
to `subprocess_runner.ts`, next to the existing
`STREAMLIB_XPC_SERVICE_NAME` reads.

## Remaining open questions

These need input before or during the follow-up implementation issue.
The research recommendation doesn't hinge on them.

1. **Multi-FD messages for NV12-style multi-plane buffers.**
   `streamlib-broker::{send,recv}_message_with_fd` are single-FD
   helpers. Some multi-plane formats (e.g. NV12 under
   `VK_EXT_image_drm_format_modifier`, occasionally under
   `VK_EXTERNAL_MEMORY_HANDLE_TYPE_DMA_BUF_BIT_EXT` disjoint-plane
   allocations) require 2 FDs per buffer (Y plane, UV plane). The
   streamlib NV12 path today uses a *single* backing allocation with
   plane offsets (contiguous NV12), so one FD is sufficient — confirm
   that holds for every format we expect polyglot processors to need.
   If any format requires disjoint planes, the broker helpers need to
   grow a multi-FD variant (widen the `cmsg` buffer, take `&[RawFd]`).

2. **Lifetime on subprocess crash — is the broker's refcount cleanup
   sufficient?** Broker's `prune_dead_runtimes`
   (`streamlib-broker/src/main.rs:110-120, 202-217`) runs every 30s
   and releases surfaces registered under dead runtimes. For polyglot
   subprocesses, "dead runtime" is the socket-close EOF, and the
   broker will drop the subprocess's `check_out` refs. But any
   iceoryx2 frame already in flight that referenced a `surface_id`
   may outlive the subprocess — a downstream host processor reading
   that `surface_id` after the producer subprocess died would hit a
   "surface gone" error at `check_out`. Is that the intended
   behavior, or does iceoryx2 frame release need to participate in
   broker refcounting? Same question exists for macOS today — flag
   as follow-up if not already tracked.

## Test gate for the follow-up implementation issue

Handoff list, not a decision point. The follow-up implementation ticket
should require:

- **Host-Rust unit test:** broker `check_in` / `check_out` roundtrip
  with a DMA-BUF FD, asserting the received FD imports to the same
  underlying memory. May already exist for the host-internal
  `SurfaceStore` path — reuse / extend rather than duplicating.
- **Python subprocess integration test:** spawn a Python subprocess,
  have it `resolve_surface(surface_id)` against a host-prepared
  surface, import into a subprocess-owned Vulkan device, read back
  first pixel matching a host-generated test pattern.
- **Deno subprocess integration test:** same, for Deno.
- **`vulkan-video-roundtrip`-style PNG E2E** per `docs/testing.md`:
  run a real pipeline with a polyglot consumer processor sampling a
  host camera output. Read PNG samples with the Read tool per the
  testing guide.
- **Negative test:** start a subprocess with `STREAMLIB_BROKER_SOCKET`
  pointing at a path that doesn't exist. Confirm `resolve_surface`
  fails with a clear error message, and that the subprocess doesn't
  crash at startup (lazy-connect decision above).

## Related

- Parent umbrella: #319 (GPU capability-based access)
- Follow-up to: #325 (pool-IDs-only polyglot escalate)
- Gated research for: `docs/design/gpu-capability-sandbox.md` §8.Q4
- Files likely touched by the follow-up implementation issue:
  - `libs/streamlib-python-native/src/lib.rs` (Linux broker shim at
    the existing `#[cfg(not(target_os = "macos"))]` stubs, lines
    1339-1381)
  - `libs/streamlib-deno-native/src/lib.rs` (same, lines 1278-1308)
  - `libs/streamlib-python/python/streamlib/subprocess_runner.py:106`
    (add `STREAMLIB_BROKER_SOCKET` read alongside the existing XPC
    read)
  - `libs/streamlib-deno/subprocess_runner.ts:143` (same)
  - `libs/streamlib-python/python/streamlib/gpu_surface.py:95`
    (remove Linux `NotImplementedError`)
  - `libs/streamlib/src/core/compiler/compiler_ops/spawn_python_native_subprocess_op.rs:125-138`
    (forward `STREAMLIB_BROKER_SOCKET` in the spawn command env if not
    already inherited)
  - Possibly `libs/streamlib-broker/src/unix_socket_service.rs` for
    multi-FD support if Open Question 2 answers "yes, NV12 now"
