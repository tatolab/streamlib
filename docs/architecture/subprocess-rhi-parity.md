# Subprocess RHI parity

> **Living document.** Validate, update, critique freely per
> [CLAUDE.md's markdown editing rules](../../CLAUDE.md#editing-markdown-documentation).
> Verify against current code before generalizing.

## Decision

Subprocess Vulkan code does **DMA-BUF / OPAQUE_FD FD import + bind + map**,
nothing else. Every privileged primitive — allocation, modifier choice,
compute kernel construction + dispatch, queue submit, fence management,
swapchain — escalates via IPC to the host's `GpuContextFullAccess`.
Bug-fix fan-out is exactly 1: a fix in `libs/streamlib-engine/src/vulkan/rhi/`
reaches every consumer (host adapter, host pipeline, subprocess via
escalate IPC).

This matches the model every comparable system has converged on
(Chromium GPU process / Dawn Wire, Unreal RHI + Shader Compile
Workers, VST3 plugin sandbox, WebGPU/wgpu-core).

## The carve-out

A subprocess can't share a host's `VkDevice` across processes — it must
construct its own consumer-only `VkDevice` to bind imported FDs. The
carve-out exists to make that bind possible and nothing more:

- DMA-BUF FD import + bind + map (`vkImportMemoryFdKHR`,
  `vkBindBufferMemory`, `vkBindImageMemory`).
- OPAQUE_FD memory import for Vulkan-aware peer importers (CUDA via
  `cudaImportExternalMemory(OPAQUE_FD)`, peer `VkInstance`s); the wire
  format carries `handle_type: "dma_buf" | "opaque_fd"` so surface-share
  lookup picks the right import path.
- Tiled-image import via `VK_IMAGE_TILING_DRM_FORMAT_MODIFIER_EXT` (the
  modifier comes from the host descriptor).
- Layout transitions on imported handles (single-shot at acquire/release
  boundary).
- Sync wait/signal on imported timeline semaphores.
- **Cross-process layout coordination via QFOT.** Subprocess Vulkan code
  may issue queue-family-ownership-transfer barriers with
  `srcQueueFamily` / `dstQueueFamily = VK_QUEUE_FAMILY_EXTERNAL` (core
  Vulkan 1.1, promoted from `VK_KHR_external_memory` — always available)
  and chain `VkExternalMemoryAcquireUnmodifiedEXT` on the consumer-side
  acquire (optional extension `VK_EXT_external_memory_acquire_unmodified`)
  so producer-side content survives the transfer per Vulkan spec. The
  acquire-side extension is the only meaningful gate, optionally probed
  at `ConsumerVulkanDevice::new` / `HostVulkanDevice::new`; when missing
  the helpers fall back to a bridging `UNDEFINED → target` transition
  (content discard permitted by spec, preserved in practice on every
  modern Linux Vulkan driver). The release/acquire helpers
  (`release_to_foreign`, `acquire_from_foreign`) live on both
  `ConsumerVulkanDevice` and `HostVulkanDevice` and are exposed via the
  `VulkanRhiDevice` trait so adapter code generic over device flavor
  works unchanged. NVIDIA Linux does not expose
  `VK_EXT_external_memory_acquire_unmodified`; the bridging fallback is
  the structurally permanent path on NVIDIA, with QFOT-acquire reserved
  for Mesa drivers.

Lives in the standalone [`streamlib-consumer-rhi`][crate] crate.
Cdylibs (`streamlib-python-native`, `streamlib-deno-native`) depend on
this crate, NOT the full `streamlib`, so the FullAccess capability
boundary is enforced by the type system — a cdylib's dep graph excludes
`streamlib` and physically cannot reach `HostVulkanDevice`, the host
VMA pools, the modifier probe, or any other privileged primitive.

[crate]: ../../libs/streamlib-consumer-rhi/

## Single-pattern principle

Every surface adapter rides the same shape:

- The adapter crate (`streamlib-adapter-vulkan`,
  `streamlib-adapter-opengl`, `streamlib-adapter-cpu-readback`,
  `streamlib-adapter-skia`, `streamlib-adapter-cuda`) is **generic over
  `D: VulkanRhiDevice`** from `streamlib-consumer-rhi`.
- **Host setup** instantiates the adapter against a host-flavor device;
  pre-allocates whatever per-surface resources the adapter needs (an
  exportable `VkImage` for vulkan/opengl/skia; an exportable HOST_VISIBLE
  staging `VkBuffer` + a timeline semaphore for cpu-readback; an
  OPAQUE_FD HOST_VISIBLE `VkBuffer` for cuda) via the host RHI;
  registers via surface-share.
- **Subprocess setup** looks the registration up via surface-share,
  imports the FDs through `ConsumerVulkanTexture` /
  `ConsumerVulkanBuffer` / `ConsumerVulkanTimelineSemaphore`, and
  instantiates the **same** adapter type against a consumer-flavor
  device. Same trait surface, same acquire/release shape.
- **Per-acquire IPC**, if the adapter needs the host to do work
  (cpu-readback's `vkCmdCopyImageToBuffer`, escalated compute dispatch
  via `register_compute_kernel` + `run_compute_kernel`), is a **thin
  trigger** — "do the work, signal this timeline value when done" — and
  the subprocess waits on the imported timeline through the carve-out,
  not on a fresh FD-passing payload.

The single-pattern principle is the engine-model rule
([CLAUDE.md "The StreamLib Engine Model"](../../CLAUDE.md#the-streamlib-engine-model))
applied to the surface-adapter layer: there is ONE way to expose a
host-allocated GPU resource to a subprocess customer, and every adapter
uses it. RHI bug fixes (e.g. import-side memory-type selection,
layout-transition pipeline-stage masks, timeline-semaphore wait
timeouts) propagate to every adapter automatically because all flow
through the same `consumer-rhi` types.

## Per-pattern decisions

| Pattern | Where | How subprocess gets RHI fixes for free |
| --- | --- | --- |
| VMA pool isolation, exportable allocation | Host-only | Host allocates; subprocess imports the FD |
| EGL DRM-modifier probe (NVIDIA tile-required) | Host-only | Host chooses; subprocess imports tiled |
| Pre-swapchain allocation order (NVIDIA cap) | Host-only | Subprocess never allocates exportable memory |
| Per-queue submit mutex | Host-only | Subprocess holds no `VkQueue` |
| Frames-in-flight=2 sizing | Host-only | Subprocess has no swapchain |
| `VulkanComputeKernel` SPIR-V reflection + dispatch | Escalate IPC | `register_compute_kernel` + `run_compute_kernel` |
| Graphics-pipeline draw | Escalate IPC | `register_graphics_kernel` + `run_graphics_draw` |
| Ray-tracing AS build + trace | Escalate IPC | `register_acceleration_structure_blas` / `_tlas` + `register_ray_tracing_kernel` + `run_ray_tracing_kernel` |
| `vkCmdCopyImageToBuffer` for cpu-readback | Escalate IPC (thin trigger only; staging buffers + timeline pre-registered via surface-share) | Subprocess imports the staging buffer + timeline through `ConsumerVulkanBuffer` / `ConsumerVulkanTimelineSemaphore` once at registration, then per-acquire is `run_cpu_readback_copy(surface_id) → done(timeline_value)` plus a consumer-side wait |
| Layout transitions / timeline waits beyond carve-out | Host-only | Adapter runs at acquire/release boundary |
| Validation layers + tracing | Host-only | Subprocess uses `tracing::*!` macros via escalate `log` op |
| Single `VkDevice` per process (NVIDIA dual-device crash) | Host has `FullAccess` device; subprocess has consumer-only device | Crash triggers on *concurrent submission*; subprocess submits nothing — provably safe ([learning](../learnings/nvidia-dual-vulkan-device-crash.md)) |
| DMA-BUF FD import + bind + map | **Carve-out** (host AND subprocess) | One shared crate (`streamlib-consumer-rhi`) |
| Tiled-image import (`VK_IMAGE_TILING_DRM_FORMAT_MODIFIER_EXT`) | **Carve-out** | Same crate |
| HOST_VISIBLE staging-buffer import (cpu-readback) | **Carve-out** | Same crate (`ConsumerVulkanBuffer`) |
| OPAQUE_FD VkBuffer import (cuda) | **Carve-out** | Same crate (`ConsumerVulkanDevice::import_opaque_fd_memory` + `ConsumerVulkanBuffer::from_opaque_fd`). The cdylib re-exports the same FD into CUDA via `cudaImportExternalMemory(OPAQUE_FD)` → `cudaExternalMemoryGetMappedBuffer`. OPAQUE_FD is not interchangeable with DMA-BUF: DLPack consumers (PyTorch / JAX / NumPy `from_dlpack`) require a flat `void*` device pointer, and only `cudaExternalMemoryGetMappedBuffer` produces one — and that requires the source memory to be a `VkBuffer` exported as OPAQUE_FD, not DMA-BUF |

## Layered architecture

```
┌──────────────────────────────────────────────────────────────────────┐
│ HOST PROCESS                                                         │
│  ╔══════════════════════════════════════════════════════════════╗    │
│  ║  streamlib RHI  (libs/streamlib-engine/src/vulkan/rhi/)      ║    │
│  ║  Host-side wins live here — VulkanComputeKernel,             ║    │
│  ║  VulkanGraphicsKernel, VulkanRayTracingKernel, VMA pools,    ║    │
│  ║  queue mutex, modifier probe, frames-in-flight=2,            ║    │
│  ║  HostVulkanDevice + Host* RHI types                          ║    │
│  ╚══════════════════════════════════════════════════════════════╝    │
│       ▲                                                              │
│  ┌────┴──────────────────────────────────────────────────────────┐   │
│  │ streamlib-consumer-rhi  (standalone crate)                    │   │
│  │ ConsumerVulkanDevice, ConsumerVulkan{Texture,Buffer,          │   │
│  │ TimelineSemaphore}, VulkanRhiDevice / DevicePrivilege /       │   │
│  │ VulkanTextureLike / VulkanRhiBuffer /                         │   │
│  │ VulkanTimelineSemaphoreLike trait machinery,                  │   │
│  │ TextureFormat / TextureUsages / PixelFormat, VulkanLayout     │   │
│  │ ✓ Capability boundary TYPE-SYSTEM enforced                    │   │
│  └───────────────────────────────────────────────────────────────┘   │
│       ▲      ▲      ▲      ▲      ▲                                  │
│  ┌────┴──┬───┴──┬───┴────┬─┴────┬─┴────┐                             │
│  │ vk-   │ gl-  │cpu-rb- │cuda- │skia- │ All adapters ride           │
│  │ adptr │adptr │adptr   │adptr │adptr │ consumer-rhi. Each is       │
│  │       │      │        │      │      │ generic over                │
│  │       │      │        │      │      │ D: VulkanRhiDevice; host    │
│  │       │      │        │      │      │ uses HostVulkanDevice,      │
│  │       │      │        │      │      │ cdylib uses                 │
│  │       │      │        │      │      │ ConsumerVulkanDevice.       │
│  └───────┴──────┴────────┴──────┴──────┘                             │
│       ▲      ▲      ▲      ▲      ▲     Pre-registered surfaces via  │
│       │ surface-share check_in (one-shot DMA-BUF / OPAQUE_FD +       │
│       │ timeline FD passing); per-acquire IPC reduces to a thin      │
│       │ trigger when host work is required.                          │
└───────┼──────────────────────────────────────────────────────────────┘
        ▼
┌──────────────────────┐         ┌──────────────────────┐
│ PYTHON SUBPROC       │         │ DENO SUBPROC         │
│ Cargo: consumer-rhi  │         │ Cargo: consumer-rhi  │
│  + adapter-{abi,     │         │  + adapter-{abi,     │
│   vulkan, opengl,    │         │   vulkan, opengl,    │
│   cpu-readback,      │         │   cpu-readback,      │
│   cuda, skia};       │         │   cuda, skia};       │
│  NOT full streamlib  │         │  NOT full streamlib  │
└──────────────────────┘         └──────────────────────┘
```

`cargo tree -p streamlib-{python,deno}-native | grep -c "^streamlib v"`
returns 0 — the capability boundary is enforced by Cargo dep resolution
itself.

## Trip-wires

Revisit when:

1. **An adapter wants to bypass the single-pattern shape** (e.g. "we
   don't need consumer-rhi for X because Y"). Default answer is no; the
   engine-model rule is one shape for all surface adapters.
2. **Subprocess wants to author a kernel from raw SPIR-V at runtime** —
   extend the escalate kernel-register ops, do not mirror
   `VulkanComputeKernel` / `VulkanGraphicsKernel` /
   `VulkanRayTracingKernel` in the subprocess.
3. **Subprocess wants to allocate** beyond what import covers — escalate
   the allocation; do not lift the carve-out into an export-side one.
4. **`run_compute_kernel` / `run_cpu_readback_copy` shows up in profiles
   at frame rate** — batch triggers before reaching for shared-memory
   rings.
5. **Host-side fix can't fan out via escalate IPC** (e.g. driver
   workaround needed on consumer-side `VkDevice`) — carve-out absorbs
   it; document the exception.

## Related

- [adapter-runtime-integration.md](adapter-runtime-integration.md) —
  *how* a subprocess obtains an adapter context.
- [adapter-authoring.md](adapter-authoring.md) — implementation
  contract for new surface adapters.
- [compute-kernel.md](compute-kernel.md) — host's `VulkanComputeKernel`.
- [graphics-kernel.md](graphics-kernel.md) — host's
  `VulkanGraphicsKernel`.
- [ray-tracing-kernel.md](ray-tracing-kernel.md) — host's
  `VulkanRayTracingKernel`.
- [`.claude/workflows/polyglot.md`](../../.claude/workflows/polyglot.md)
  — workflow rule the carve-out lives under.
- [`.claude/workflows/adapter.md`](../../.claude/workflows/adapter.md) —
  auto-loaded for `adapter`-labeled work.
- [`docs/learnings/`](../learnings/) — bug evidence motivating one host
  VkDevice.
