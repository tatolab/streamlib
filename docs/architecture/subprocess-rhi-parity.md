# Subprocess RHI parity

> **Living document.** Validate, update, critique freely per
> [CLAUDE.md's markdown editing rules](../../CLAUDE.md#editing-markdown-documentation).
> Reflects code state as of 2026-04-28 (post-#560 Phase 2). Verify
> against current code before generalizing.
>
> **2026-04-28 — Architectural correction.** The "cpu-readback is
> escalate-IPC-only" classification in earlier revisions of this doc
> was an architectural drift. Every surface adapter — including
> cpu-readback — rides `streamlib-consumer-rhi`'s carve-out for
> staging buffers + timeline imports; per-acquire IPC (when host work
> is required) is a thin trigger, not a bespoke FD-passing path. See
> the [Single-pattern principle](#single-pattern-principle-2026-04-28)
> section below and the cpu-readback rewire issue tracked under
> milestone *Surface Adapter Architecture*.

## Decision

Subprocess Vulkan code does **DMA-BUF FD import + bind + map**, nothing
else. Every privileged primitive — allocation, modifier choice,
compute kernel construction + dispatch, queue submit, fence management,
swapchain — escalates via IPC to the host's `GpuContextFullAccess`.
Bug-fix fan-out is exactly 1: a fix in `libs/streamlib/src/vulkan/rhi/`
reaches every consumer (host adapter, host pipeline, subprocess via
escalate IPC).

This matches the model every comparable system has converged on
(Chromium GPU process / Dawn Wire, Unreal RHI + Shader Compile
Workers, VST3 plugin sandbox, WebGPU/wgpu-core).

## The carve-out

A subprocess can't share a host's `VkDevice` across processes — it must
construct its own consumer-only `VkDevice` to bind imported FDs. The
carve-out exists to make that bind possible and nothing more:

- DMA-BUF FD import + bind + map (`vkImportMemoryFdKHR`, `vkBindBufferMemory`, `vkBindImageMemory`).
- Tiled-image import via `VK_IMAGE_TILING_DRM_FORMAT_MODIFIER_EXT` (the modifier comes from the host descriptor).
- Layout transitions on imported handles (single-shot at acquire/release boundary).
- Sync wait/signal on imported timeline semaphores.
- **Cross-process layout coordination via QFOT (#633).** Subprocess
  Vulkan code may issue queue-family-ownership-transfer barriers with
  `srcQueueFamily` / `dstQueueFamily = VK_QUEUE_FAMILY_EXTERNAL`
  (core Vulkan 1.1, promoted from `VK_KHR_external_memory` — always
  available) and chain `VkExternalMemoryAcquireUnmodifiedEXT` on the
  consumer-side acquire (optional extension
  `VK_EXT_external_memory_acquire_unmodified`) so producer-side
  content survives the transfer per Vulkan spec. The acquire-side
  extension is the only meaningful gate, optionally probed at
  `ConsumerVulkanDevice::new` / `HostVulkanDevice::new`; when missing
  the helpers fall back to a bridging `UNDEFINED → target` transition
  (content discard permitted by spec, preserved in practice on every
  modern Linux Vulkan driver). The release/acquire helpers
  (`release_to_foreign`, `acquire_from_foreign`) live on both
  `ConsumerVulkanDevice` and `HostVulkanDevice` and are exposed via
  the `VulkanRhiDevice` trait so adapter code generic over device
  flavor works unchanged. To the best of our current knowledge as of
  2026-05-03, NVIDIA Linux does not expose
  `VK_EXT_external_memory_acquire_unmodified` and isn't on track to
  add it; the bridging fallback is the structurally permanent path on
  NVIDIA, with QFOT-acquire reserved for Mesa drivers.

Lives in the standalone [`streamlib-consumer-rhi`][crate] crate (post-#560).
Cdylibs (`streamlib-python-native`, `streamlib-deno-native`) depend
on this crate, NOT the full `streamlib`, so the FullAccess capability
boundary is enforced by the type system — a cdylib's dep graph
excludes `streamlib` and physically cannot reach `HostVulkanDevice`,
the host VMA pools, the modifier probe, or any other privileged
primitive.

[crate]: ../../libs/streamlib-consumer-rhi/

## Single-pattern principle (2026-04-28)

Every surface adapter rides the same shape:

- The adapter crate (`streamlib-adapter-vulkan`,
  `streamlib-adapter-opengl`, `streamlib-adapter-cpu-readback`,
  `streamlib-adapter-skia`) is **generic over `D: VulkanRhiDevice`**
  from `streamlib-consumer-rhi`.
- **Host setup** instantiates the adapter against a host-flavor
  device; pre-allocates whatever per-surface resources the adapter
  needs (an exportable `VkImage` for vulkan/opengl/skia; an
  exportable HOST_VISIBLE staging `VkBuffer` + a timeline semaphore
  for cpu-readback) via the host RHI; registers via surface-share.
- **Subprocess setup** looks the registration up via surface-share,
  imports the FDs through `ConsumerVulkanTexture` /
  `ConsumerVulkanPixelBuffer` / `ConsumerVulkanTimelineSemaphore`,
  and instantiates the **same** adapter type against a
  consumer-flavor device. Same trait surface, same acquire/release
  shape.
- **Per-acquire IPC**, if the adapter needs the host to do work
  (cpu-readback's `vkCmdCopyImageToBuffer`, escalated compute
  dispatch from #550), is a **thin trigger** — "do the work, signal
  this timeline value when done" — and the subprocess waits on the
  imported timeline through the carve-out, not on a fresh FD-passing
  payload.

The single-pattern principle is the engine-model rule
([CLAUDE.md "The StreamLib Engine Model"](../../CLAUDE.md#the-streamlib-engine-model))
applied to the surface-adapter layer: there is ONE way to expose a
host-allocated GPU resource to a subprocess customer, and every
adapter uses it. RHI bug fixes (e.g. import-side memory-type
selection, layout-transition pipeline-stage masks, timeline-semaphore
wait timeouts) propagate to every adapter automatically because all
three flow through the same `consumer-rhi` types.

## Per-pattern decisions

| Pattern | Where | How subprocess gets RHI fixes for free |
| --- | --- | --- |
| VMA pool isolation, exportable allocation | Host-only | Host allocates; subprocess imports the FD |
| EGL DRM-modifier probe (NVIDIA tile-required) | Host-only | Host chooses; subprocess imports tiled |
| Pre-swapchain allocation order (NVIDIA cap) | Host-only | Subprocess never allocates exportable memory |
| Per-queue submit mutex | Host-only | Subprocess holds no `VkQueue` |
| Frames-in-flight=2 sizing | Host-only | Subprocess has no swapchain |
| `VulkanComputeKernel` SPIR-V reflection + dispatch | Escalate IPC (#550) | `RegisterComputeKernel` + `RunComputeKernel` |
| **`vkCmdCopyImageToBuffer` for cpu-readback** | **Escalate IPC (thin trigger only; staging buffers + timeline pre-registered via surface-share)** | **Subprocess imports the staging buffer + timeline through `ConsumerVulkanPixelBuffer` / `ConsumerVulkanTimelineSemaphore` once at registration, then per-acquire is `RunCpuReadbackCopy(surface_id) → done(timeline_value)` plus a consumer-side wait** |
| Layout transitions / timeline waits beyond carve-out | Host-only | Adapter runs at acquire/release boundary |
| Validation layers + tracing | Host-only | Subprocess uses `tracing::*!` macros via escalate `log` op |
| Single `VkDevice` per process (NVIDIA dual-device crash) | Host has `FullAccess` device; subprocess has consumer-only device | Crash triggers on *concurrent submission*; subprocess submits nothing — provably safe ([learning](../learnings/nvidia-dual-vulkan-device-crash.md)) |
| DMA-BUF FD import + bind + map | **Carve-out** (host AND subprocess) | One shared crate (`streamlib-consumer-rhi` post-#560) |
| Tiled-image import (`VK_IMAGE_TILING_DRM_FORMAT_MODIFIER_EXT`) | **Carve-out** | Same crate |
| HOST_VISIBLE staging-buffer import (cpu-readback) | **Carve-out** | Same crate (`ConsumerVulkanPixelBuffer`) |
| **OPAQUE_FD VkBuffer import (cuda — #588)** | **Carve-out** | **Same crate (`ConsumerVulkanDevice::import_opaque_fd_memory` + `ConsumerVulkanPixelBuffer::from_opaque_fd`); the cdylib re-exports the same FD into CUDA via `cudaImportExternalMemory(OPAQUE_FD)` → `cudaExternalMemoryGetMappedBuffer`. OPAQUE_FD is not interchangeable with DMA-BUF: DLPack consumers (PyTorch / JAX / NumPy `from_dlpack`) require a flat `void*` device pointer, and only `cudaExternalMemoryGetMappedBuffer` produces one — and that requires the source memory to be a `VkBuffer` exported as OPAQUE_FD, not DMA-BUF. The wire format carries `handle_type: "dma_buf" \| "opaque_fd"` so surface-share lookup picks the right import path** |

## Today (post-#560 Phase 2 + #562 cpu-readback rewire + #588 cuda OPAQUE_FD plumbing)

> Updated 2026-04-28 — #562 cpu-readback rewire (Path E) landed.
> The cdylib swap to `ConsumerVulkanDevice` and the
> `streamlib-consumer-rhi` crate extraction now hold for **all three
> active adapters** (Vulkan, OpenGL, cpu-readback).
>
> ~~Skia (#513) is frozen until its host crate ships and will land
> directly on the single-pattern shape from day one.~~ — Superseded
> 2026-04-29: all five P0s (#550, #551, #552, #553, #555) closed, so
> #513 and #515 unfrozen the same day. Skia and the processor-port
> refactor are now eligible to start; both must still land on the
> single-pattern shape.
>
> Updated 2026-04-30 — #588 OPAQUE_FD plumbing chain landed
> (`RhiExternalHandle::OpaqueFd` variant, host-side polymorphic
> export through `RhiPixelBufferExport`, surface-share `handle_type`
> wire-format discriminator, `ConsumerVulkanDevice::import_opaque_fd_memory`
> + `ConsumerVulkanPixelBuffer::from_opaque_fd`, end-to-end
> integration test, DLPack capsule shape on `CudaReadView` /
> `CudaWriteView`, `cudaPointerGetAttributes` probe). The
> `streamlib-adapter-cuda` adapter (#587 / #588) is now the
> **fourth** consumer of the single-pattern shape — same generic
> over `D: VulkanRhiDevice`, same surface-share registration, same
> consumer-rhi import; the only twist is that the host pre-registers
> the staging buffer as OPAQUE_FD instead of DMA-BUF (DLPack
> requires a flat `void*` from `cudaExternalMemoryGetMappedBuffer`,
> which requires OPAQUE_FD). The diagram below is from the
> 2026-04-28 snapshot — cuda fits the same `vk-adptr` / `gl-adptr`
> / `cpu-rb-adptr` row; mentally insert a `cuda-adptr` box, and add
> `mod cuda` to the per-cdylib module list (currently empty
> pending #589/#590).

```
┌──────────────────────────────────────────────────────────────────────┐
│ HOST PROCESS                                                         │
│  ╔══════════════════════════════════════════════════════════════╗    │
│  ║  streamlib RHI  (libs/streamlib/src/vulkan/rhi/)             ║    │
│  ║  Host-side wins live here — VulkanComputeKernel, VMA pools,  ║    │
│  ║  queue mutex, modifier probe, frames-in-flight=2,            ║    │
│  ║  HostVulkanDevice + Host* RHI types                          ║    │
│  ╚══════════════════════════════════════════════════════════════╝    │
│       ▲                                                              │
│  ┌────┴──────────────────────────────────────────────────────────┐   │
│  │ streamlib-consumer-rhi (#560 — standalone crate)              │   │
│  │ ConsumerVulkanDevice, ConsumerVulkan{Texture,PixelBuffer,     │   │
│  │ TimelineSemaphore}, VulkanRhiDevice / DevicePrivilege /       │   │
│  │ VulkanTextureLike / VulkanPixelBufferLike /                   │   │
│  │ VulkanTimelineSemaphoreLike trait machinery,                  │   │
│  │ TextureFormat / TextureUsages / PixelFormat                   │   │
│  │ ✓ Capability boundary TYPE-SYSTEM enforced                    │   │
│  └───────────────────────────────────────────────────────────────┘   │
│       ▲      ▲      ▲       (skia in flight, #513 — unfrozen 04-29)  │
│  ┌────┴──┬───┴──┬───┴────────┐                                       │
│  │ vk-   │ gl-  │cpu-rb-     │  All three adapters ride consumer-rhi.│
│  │ adptr │adptr │adptr       │  Each is generic over                 │
│  │       │      │            │  D: VulkanRhiDevice; the host         │
│  │       │      │            │  instantiates against HostVulkan-     │
│  │       │      │            │  Device, the cdylib against           │
│  │       │      │            │  ConsumerVulkanDevice.                │
│  └───────┴──────┴────────────┘                                       │
│       ▲      ▲      ▲           Pre-registered surfaces via          │
│       │ surface-share check_in (one-shot DMA-BUF + timeline FD       │
│       │ passing); per-acquire IPC reduces to a thin trigger when     │
│       │ host work is required (cpu-readback's vkCmdCopyImageToBuffer │
│       │ via `run_cpu_readback_copy`; compute via #550 once landed).  │
└───────┼──────────────────────────────────────────────────────────────┘
        ▼
┌──────────────────────┐         ┌──────────────────────┐
│ PYTHON SUBPROC       │         │ DENO SUBPROC         │
│  mod vulkan          │         │  mod vulkan          │
│  mod opengl          │         │  mod opengl          │
│  mod cpu_readback    │         │  mod cpu_readback    │ ← #562: shape
│ ✗vulkan_compute_     │         │ ✗vulkan_compute_     │ ← raw vulkan
│  dispatch (~200 LOC) │         │  dispatch (~200 LOC) │   × 2 cdylibs,
│                      │         │                      │   #550 open
│ ✗surface_share_      │         │ ✗surface_share_      │ ← legacy,
│  vulkan_linux (~280) │         │  vulkan_linux (~280) │   #553 open
│ ✓Cargo: consumer-rhi │         │ ✓Cargo: consumer-rhi │ ← capability
│  + adapter-{abi,    │         │  + adapter-{abi,    │   boundary
│   vulkan, opengl,   │         │   vulkan, opengl,   │   ENFORCED for
│   cpu-readback};    │         │   cpu-readback};    │   all three
│  NOT full streamlib │         │  NOT full streamlib │   adapters
└──────────────────────┘         └──────────────────────┘
```

`cargo tree -p streamlib-{python,deno}-native | grep -c "^streamlib v"` → **0** (assertion holds for all three adapters post-#562).

## Open follow-ups (after #562 lands)

> Updated 2026-04-29 — every P0 below closed. Section preserved with
> strike-throughs so future readers can see the original gating chain.

- ~~**#550** [P0] — escalate-IPC `RegisterComputeKernel` +
  `RunComputeKernel`; retire the `vulkan_compute_dispatch` raw-vulkan
  helper inside each cdylib (≈200 LOC × 2 still in tree).~~ Closed
  2026-04-29 (#571).
- ~~**#553** [P0] — retire `surface_share_vulkan_linux` (≈280 LOC × 2)
  from the cdylibs once #550 covers compute and #551 covers
  registration.~~ Closed 2026-04-29 (#573).
- ~~**#551** [P0] — pull the registration `Registry<T: SurfaceRegistration>`
  into `streamlib-adapter-abi` so adapter crates stop redoing the same
  per-surface book-keeping.~~ Closed 2026-04-28.
- ~~**#555** [P0] — CI boundary-grep as defense in depth around the
  type-system boundary. Must include "no cdylib transitively pulls
  the full `streamlib` crate" plus "no adapter crate's runtime
  `[dependencies]` lists `streamlib`" — covers cpu-readback once it
  lands the rewire.~~ Closed 2026-04-29 (#570 + tightening in #574).
- ~~**#556** [P1] — adapter-authoring blueprint, codifies the
  single-pattern shape so future adapters land on the right shape
  by default.~~ Landed 2026-04-30 — see
  [`adapter-authoring.md`](adapter-authoring.md).
- **#513** (skia adapter), **#515** (processor-port refactor) —
  ~~`frozen` until the P0s above land.~~ Unfrozen 2026-04-29 once
  the P0 chain closed. Skia must follow the single-pattern shape
  from day one.

## Trip-wires

Revisit when:

1. **An adapter wants to bypass the single-pattern shape** (e.g. "we don't need consumer-rhi for X because Y") — that's the cpu-readback drift recurring. Default answer is no; the engine-model rule is one shape for all surface adapters.
2. **Subprocess wants to author a kernel from raw SPIR-V at runtime** — extend `RegisterComputeKernel`, do not mirror `VulkanComputeKernel` in the subprocess.
3. **Subprocess wants to allocate** beyond what import covers — escalate the allocation; do not lift the carve-out into an export-side one.
4. **`RunComputeKernel` / `RunCpuReadbackCopy` shows up in profiles at frame rate** — batch triggers before reaching for shared-memory rings.
5. **Host-side fix can't fan out via escalate IPC** (e.g. driver workaround needed on consumer-side `VkDevice`) — carve-out absorbs it; document the exception.

## Follow-up issues

Milestone *Surface Adapter Architecture* (#16):

- ~~**#550** [P0] — escalate-IPC `RegisterComputeKernel` + `RunComputeKernel`; on-disk pipeline cache; retire `vulkan_compute_dispatch`.~~ Closed 2026-04-29 (#571).
- ~~**#551** [P0] — extract `Registry<T: SurfaceRegistration>` into `streamlib-adapter-abi`.~~ Closed 2026-04-28.
- ~~**#552** [P0] — promote `streamlib::adapter_support` → `streamlib-consumer-rhi` crate.~~ Closed 2026-04-28.
- ~~**#553** [P0] — retire `surface_share_vulkan_linux` from natives.~~ Closed 2026-04-29 (#573).
- ~~**#555** [P0] — boundary-grep CI check.~~ Closed 2026-04-29 (#570 + #574).
- ~~**#556** [P1] — adapter authoring blueprint.~~ Landed 2026-04-30
  — see [`adapter-authoring.md`](adapter-authoring.md).
- **#558** — dedicated `AdapterError::SurfaceAlreadyRegistered` variant.
- **#565** — CUDA surface adapter.
- **#513** (skia adapter), **#515** (processor-port refactor) — ~~`frozen` until P0s land.~~ Unfrozen 2026-04-29 — eligible to start.

## Related

- [adapter-runtime-integration.md](adapter-runtime-integration.md) — *how* a subprocess obtains an adapter context.
- [adapter-authoring.md](adapter-authoring.md) — implementation contract for new surface adapters.
- [compute-kernel.md](compute-kernel.md) — host's `VulkanComputeKernel`.
- [`.claude/workflows/polyglot.md`](../../.claude/workflows/polyglot.md) — workflow rule the carve-out lives under.
- [`.claude/workflows/adapter.md`](../../.claude/workflows/adapter.md) — auto-loaded by `/amos:next` for `adapter`-labeled work.
- [`docs/learnings/`](../learnings/) — bug evidence motivating one host VkDevice.
- #525 — research issue this doc closes.
