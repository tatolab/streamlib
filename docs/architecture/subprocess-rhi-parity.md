# Subprocess RHI parity

> **Living document.** Validate, update, critique freely per
> [CLAUDE.md's markdown editing rules](../../CLAUDE.md#editing-markdown-documentation).
> Reflects code state as of 2026-04-27 (post-#549). Verify against
> current code before generalizing.

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

Lives in `streamlib::adapter_support` today (convention only); graduates
to the standalone `streamlib-consumer-rhi` crate (#552) so cdylibs
physically cannot reach `FullAccess` types.

## Per-pattern decisions

| Pattern | Where | How subprocess gets RHI fixes for free |
| --- | --- | --- |
| VMA pool isolation, exportable allocation | Host-only | Host allocates; subprocess imports the FD |
| EGL DRM-modifier probe (NVIDIA tile-required) | Host-only | Host chooses; subprocess imports tiled |
| Pre-swapchain allocation order (NVIDIA cap) | Host-only | Subprocess never allocates exportable memory |
| Per-queue submit mutex | Host-only | Subprocess holds no `VkQueue` |
| Frames-in-flight=2 sizing | Host-only | Subprocess has no swapchain |
| `VulkanComputeKernel` SPIR-V reflection + dispatch | Escalate IPC (#550) | `RegisterComputeKernel` + `RunComputeKernel` |
| Layout transitions / timeline waits beyond carve-out | Host-only | Adapter runs at acquire/release boundary |
| Validation layers + tracing | Host-only | Subprocess uses `tracing::*!` macros via escalate `log` op |
| Single `VkDevice` per process (NVIDIA dual-device crash) | Host has `FullAccess` device; subprocess has consumer-only device | Crash triggers on *concurrent submission*; subprocess submits nothing — provably safe ([learning](../learnings/nvidia-dual-vulkan-device-crash.md)) |
| DMA-BUF FD import + bind + map | **Carve-out** (host AND subprocess) | One shared crate (`streamlib-consumer-rhi` post-#552) |
| Tiled-image import (`VK_IMAGE_TILING_DRM_FORMAT_MODIFIER_EXT`) | **Carve-out** | Same crate |

## Today (post-#549, pre-P0s)

```
┌──────────────────────────────────────────────────────────────────────┐
│ HOST PROCESS                                                         │
│  ╔══════════════════════════════════════════════════════════════╗    │
│  ║  streamlib RHI  (libs/streamlib/src/vulkan/rhi/)             ║    │
│  ║  All wins live here — VulkanComputeKernel, VMA pools, queue  ║    │
│  ║  mutex, modifier probe, frames-in-flight=2                   ║    │
│  ╚══════════════════════════════════════════════════════════════╝    │
│       ▲                                                              │
│  ┌────┴──────────────────────────────────────────────────────────┐   │
│  │ streamlib::adapter_support  (re-export, convention only)      │   │
│  │ ⚠  cdylibs link FULL streamlib — boundary not type-enforced   │   │
│  └───────────────────────────────────────────────────────────────┘   │
│       ▲      ▲      ▲     (skia frozen)                              │
│  ┌────┴──┬───┴──┬───┴────────┐                                       │
│  │ vk-   │ gl-  │cpu-rb-     │  each adapter rolls its own          │
│  │ adptr │adptr │adptr       │  try_begin_read/write — ~50 LOC × 3  │
│  └───────┴──────┴────────────┘                                       │
│       ▲ surface-share + escalate IPC (no compute ops)                │
└───────┼──────────────────────────────────────────────────────────────┘
        ▼
┌──────────────────────┐         ┌──────────────────────┐
│ PYTHON SUBPROC       │         │ DENO SUBPROC         │
│  mod vulkan          │ ✓ #549  │  mod vulkan          │
│  mod opengl          │ ✓ #530  │  mod opengl          │
│ ✗vulkan_compute_     │         │ ✗vulkan_compute_     │ ← raw vulkan
│  dispatch (~200 LOC) │         │  dispatch (~200 LOC) │   × 2 cdylibs
│ ✗surface_share_      │         │ ✗surface_share_      │ ← legacy
│  vulkan_linux (~280) │         │  vulkan_linux (~280) │   × 2 cdylibs
│ ✗Cargo: full         │         │ ✗Cargo: full         │ ← capability
│  streamlib dep       │         │  streamlib dep       │   leak risk
└──────────────────────┘         └──────────────────────┘
```

## Outcome (after #550, #551, #552, #553, #555)

```
┌──────────────────────────────────────────────────────────────────────┐
│ HOST PROCESS                                                         │
│  ╔══════════════════════════════════════════════════════════════╗    │
│  ║  streamlib RHI (unchanged single source)                     ║    │
│  ║  + on-disk pipeline cache (#550 scope)                       ║    │
│  ╚══════════════════════════════════════════════════════════════╝    │
│       ▲                                                              │
│  ┌────┴──────────────────────────────────────────────────────────┐   │
│  │ streamlib-consumer-rhi (#552 — standalone crate)              │   │
│  │ ✓ Capability boundary TYPE-SYSTEM enforced                    │   │
│  └───────────────────────────────────────────────────────────────┘   │
│       ▲      ▲      ▲      ▲                                         │
│  ┌────┴──┬───┴──┬───┴──┬───┴───┐                                     │
│  │ vk-   │ gl-  │cpu-rb│ skia  │  shared Registry<T> (#551) —        │
│  │ adptr │adptr │adptr │ #513  │  zero duplicated try_begin_*        │
│  └───────┴──────┴──────┴───────┘  (skia unfrozen)                    │
│  ╔══════════════════════════════════════════════════════════════╗    │
│  ║ Escalate IPC ops (#550)                                      ║    │
│  ║   RegisterComputeKernel(spv, bindings) → kernel_id           ║    │
│  ║   RunComputeKernel(kernel_id, surface, push, dims)           ║    │
│  ╚══════════════════════════════════════════════════════════════╝    │
│  ╔══════════════════════════════════════════════════════════════╗    │
│  ║ CI boundary-grep (#555) — defense in depth                   ║    │
│  ╚══════════════════════════════════════════════════════════════╝    │
└───────┼──────────────────────────────────────────────────────────────┘
        ▼
┌──────────────────────┐         ┌──────────────────────┐
│ PYTHON SUBPROC       │         │ DENO SUBPROC         │
│  mod vulkan          │         │  mod vulkan          │
│  mod opengl          │         │  mod opengl          │
│ ✓dispatch_compute    │         │ ✓dispatch_compute    │ ← #550: thin
│  thin escalate-IPC   │         │  thin escalate-IPC   │   IPC wrapper
│ ⊘surface_share_      │         │ ⊘surface_share_      │ ← #553: del
│  vulkan_linux DELETED│         │  vulkan_linux DELETED│
│ ✓Cargo: consumer-rhi │         │ ✓Cargo: consumer-rhi │ ← #552:
│  + adapter-{abi,*};  │         │  + adapter-{abi,*};  │   capability
│  NOT full streamlib  │         │  NOT full streamlib  │   ENFORCED
└──────────────────────┘         └──────────────────────┘
```

## Trip-wires

Revisit when:

1. **Subprocess wants to author a kernel from raw SPIR-V at runtime** — extend `RegisterComputeKernel`, do not mirror `VulkanComputeKernel` in the subprocess.
2. **Subprocess wants to allocate** beyond what import covers — escalate the allocation; do not lift the carve-out into an export-side one.
3. **`RunComputeKernel` shows up in profiles at frame rate** — batch dispatches before reaching for shared-memory rings.
4. **A new adapter's data flow isn't "static FD lives forever" or "host runs work on every acquire"** — re-derive the seam choice; see [adapter-runtime-integration.md](adapter-runtime-integration.md).
5. **Host-side fix can't fan out via escalate IPC** (e.g. driver workaround needed on consumer-side `VkDevice`) — carve-out absorbs it; document the exception.

## Follow-up issues

Milestone *Surface Adapter Architecture* (#16):

- **#550** [P0] — escalate-IPC `RegisterComputeKernel` + `RunComputeKernel`; on-disk pipeline cache; retire `vulkan_compute_dispatch`.
- **#551** [P0] — extract `Registry<T: SurfaceRegistration>` into `streamlib-adapter-abi`.
- **#552** [P0] — promote `streamlib::adapter_support` → `streamlib-consumer-rhi` crate.
- **#553** [P0] — retire `surface_share_vulkan_linux` from natives.
- **#555** [P0] — boundary-grep CI check.
- **#556** [P1] — adapter authoring blueprint.
- **#513** (skia adapter), **#515** (processor-port refactor) — `frozen` until P0s land.

## Related

- [adapter-runtime-integration.md](adapter-runtime-integration.md) — *how* a subprocess obtains an adapter context.
- [compute-kernel.md](compute-kernel.md) — host's `VulkanComputeKernel`.
- [`.claude/workflows/polyglot.md`](../../.claude/workflows/polyglot.md) — workflow rule the carve-out lives under.
- [`docs/learnings/`](../learnings/) — bug evidence motivating one host VkDevice.
- #525 — research issue this doc closes.
