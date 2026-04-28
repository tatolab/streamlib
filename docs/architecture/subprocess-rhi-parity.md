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

Lives in the standalone [`streamlib-consumer-rhi`][crate] crate (post-#560).
Cdylibs (`streamlib-python-native`, `streamlib-deno-native`) depend
on this crate, NOT the full `streamlib`, so the FullAccess capability
boundary is enforced by the type system — a cdylib's dep graph
excludes `streamlib` and physically cannot reach `HostVulkanDevice`,
the host VMA pools, the modifier probe, or any other privileged
primitive.

[crate]: ../../libs/streamlib-consumer-rhi/

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

## Today (post-#560 Phase 2)

> Updated 2026-04-28 — #560 Phase 2 landed; the cdylib swap to
> `ConsumerVulkanDevice` and the `streamlib-consumer-rhi` crate
> extraction are in. The capability boundary is type-system enforced.
> #550 (escalate-IPC compute ops) and #553 (`surface_share_vulkan_linux`
> retirement) remain open — see "Open follow-ups" below.

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
│  │ VulkanTextureLike / VulkanTimelineSemaphoreLike trait         │   │
│  │ machinery, TextureFormat / TextureUsages / PixelFormat        │   │
│  │ ✓ Capability boundary TYPE-SYSTEM enforced                    │   │
│  └───────────────────────────────────────────────────────────────┘   │
│       ▲      ▲      ▲      (skia frozen, #513)                       │
│  ┌────┴──┬───┴──┬───┴────────┐                                       │
│  │ vk-   │ gl-  │cpu-rb-     │  each adapter rolls its own          │
│  │ adptr │adptr │adptr       │  try_begin_read/write — ~50 LOC × 3  │
│  └───────┴──────┴────────────┘  (cpu-readback keeps streamlib;      │
│       ▲      ▲      ▲           others depend on consumer-rhi only) │
│       │ surface-share + escalate IPC (no compute ops)                │
└───────┼──────────────────────────────────────────────────────────────┘
        ▼
┌──────────────────────┐         ┌──────────────────────┐
│ PYTHON SUBPROC       │         │ DENO SUBPROC         │
│  mod vulkan          │         │  mod vulkan          │
│  mod opengl          │         │  mod opengl          │
│ ✗vulkan_compute_     │         │ ✗vulkan_compute_     │ ← raw vulkan
│  dispatch (~200 LOC) │         │  dispatch (~200 LOC) │   × 2 cdylibs
│ ✗surface_share_      │         │ ✗surface_share_      │ ← legacy,
│  vulkan_linux (~280) │         │  vulkan_linux (~280) │   #553 open
│ ✓Cargo: consumer-rhi │         │ ✓Cargo: consumer-rhi │ ← #560:
│  + adapter-{abi,*};  │         │  + adapter-{abi,*};  │   capability
│  NOT full streamlib  │         │  NOT full streamlib  │   ENFORCED
└──────────────────────┘         └──────────────────────┘
```

`cargo tree -p streamlib-{python,deno}-native | grep -c "^streamlib v"` → **0** (Phase 2 assertion, holds at HEAD of `main` post-#560).

## Open follow-ups (after #560 lands)

The remaining P0s in milestone #16 close out the residual technical
debt the consumer-rhi extraction made visible:

- **#550** [P0] — escalate-IPC `RegisterComputeKernel` +
  `RunComputeKernel`; retire the `vulkan_compute_dispatch` raw-vulkan
  helper inside each cdylib (≈200 LOC × 2 still in tree).
- **#553** [P0] — retire `surface_share_vulkan_linux` (≈280 LOC × 2)
  from the cdylibs once #550 covers compute and #551 covers
  registration.
- **#551** [P0] — pull the registration `Registry<T: SurfaceRegistration>`
  into `streamlib-adapter-abi` so adapter crates stop redoing the same
  per-surface book-keeping.
- **#555** [P0] — CI boundary-grep as defense in depth around the
  type-system boundary that #560 just established.
- **#556** [P1] — adapter-authoring blueprint, now that the boundary
  shape is concrete.
- **#513** (skia adapter), **#515** (processor-port refactor) —
  `frozen` until the P0s above land.

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
