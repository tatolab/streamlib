# Third-party GPU library backends — the engine-allocates / vendor-imports pattern

> **Living document.** Validate, update, and critique freely per
> [CLAUDE.md's markdown editing rules](../../CLAUDE.md#editing-markdown-documentation).
> Verify against current code before generalizing.

## What this is

This doc records the canonical shape for "a third-party GPU compute
library produces output that needs to enter streamlib's RHI as a
sampleable resource downstream consumers can read through the normal
`surface_id` contract." The library is typically vendor-specific
(NVIDIA nvJPEG, NVIDIA NVDEC, NVIDIA OptiX denoiser, AMD AMF, Intel
MFX), runs on its own GPU API surface (CUDA, vendor SDK), and has no
direct understanding of Vulkan tiling, layouts, or queue semantics.

The pattern lifts cleanly to any such library; the trade-offs are the
same; the shape is the same. **Don't re-derive it per library —
follow this doc.**

## The shape

```
                              streamlib RHI
                                   │
            host-side              │           vendor library
                                   │
   ┌─────────────────────────┐     │     ┌──────────────────────┐
   │  HostVulkanBuffer       │     │     │  e.g. CUDA context  │
   │  ::new_opaque_fd_export │ ◄───┼──── │  cudaImportExternal │
   │  (Vulkan-allocated,     │ FD  │     │      Memory(...)    │
   │  OPAQUE_FD, DEVICE_LOCAL│     │     │  cudaExternalMemory │
   │  or HOST_VISIBLE)       │     │     │      GetMappedBuffer│
   └─────────────────────────┘     │     └──────────────────────┘
              │                    │                │
              │  ┌─ vendor lib writes into the imported pointer ─┐
              │  │                                               │
              ▼  ▼                                               │
   ┌─────────────────────────┐     │                             │
   │  Vulkan timeline        │     │     ┌──────────────────────┐│
   │  semaphore (exportable) │ ◄───┼──── │  cudaImportExternal  ││
   │  signaled by vendor lib │  FD │     │      Semaphore(...)  ││
   └─────────────────────────┘     │     └──────────────────────┘│
              │                    │                              │
              ▼                    │                              │
   ┌─────────────────────────┐     │                              │
   │  vkCmdCopyBufferToImage │     │                              │
   │  → render-target        │     │                              │
   │  HostVulkanTexture in   │     │                              │
   │  TextureRing slot       │     │                              │
   │  (normal pipeline       │     │                              │
   │  Vulkan resource)       │     │                              │
   └─────────────────────────┘     │                              │
              │                    │                              │
              ▼                    │                              │
   downstream consumers read       │
   via surface_id (no awareness    │
   that vendor lib was involved)   │
```

Direction is **engine-allocates / vendor-imports** universally.
streamlib's RHI is the single gateway for GPU allocation; vendor
libraries import the FD the engine produces. This matches Granite's
`ExternalHandle` pattern, Unreal's NGX/OptiX integrations, and
Khronos's reference cross-API samples.

## Two flavors

### Buffer-flavored (vendor writes flat memory)

Use when the vendor library accepts a flat `void* / CUdeviceptr` and
writes pixels in row-major or planar order without internal tiling.
nvJPEG is the canonical example — every output format
(`NVJPEG_OUTPUT_RGBI`, `NVJPEG_OUTPUT_YUV`, etc.) writes into a
caller-supplied `unsigned char *`.

Engine resources:
- [`HostVulkanBuffer::new_opaque_fd_export_device_local`] for the
  staging surface (DEVICE_LOCAL VRAM, exported as OPAQUE_FD). The
  HOST_VISIBLE sibling [`HostVulkanBuffer::new_opaque_fd_export`]
  exists for vendors that need pinned-host access.
- [`HostVulkanTimelineSemaphore::new_exportable`] for cross-API
  sync. The vendor library signals the timeline after its writes
  complete; the Vulkan side `vkCmdCopyBufferToImage` waits on it.
- A normal `TextureRing` of render-target textures (the existing
  [`GpuContextFullAccess::create_texture_ring`] primitive). Output
  slots are non-exportable DEVICE_LOCAL; the OPAQUE_FD staging
  buffer is internal to the backend.

Per-frame steady state: vendor lib decodes into the imported
buffer → signals timeline → Vulkan-side `vkCmdCopyBufferToImage`
into the next ring slot → return the slot's `surface_id` downstream.
One Vulkan-side GPU copy per frame, no `vkAllocateMemory` on the
hot path.

### Image / mipmapped-array-flavored (vendor writes into cudaArrays)

Use when the vendor library writes into a CUDA `cudaArray_t` /
`cudaMipmappedArray_t` or its API equivalent (sampler-textured
write surface). Some image-processing libs (NPP image kernels,
OptiX texture writes, cuFFT 2D) prefer this shape because the
vendor's kernels are written against `surf2Dwrite` /
`cudaTextureObject_t` rather than flat pointers.

Engine resources:
- [`HostVulkanTexture::new_opaque_fd_export`] (`VK_IMAGE_TILING_OPTIMAL`,
  no DRM modifier, format restricted to the CUDA-mappable subset —
  `Rgba8Unorm` / `Rgba16Float` / `Rgba32Float`).
- Same timeline-semaphore primitive.
- The vendor library imports via `cudaImportExternalMemory(OPAQUE_FD)` +
  `cudaExternalMemoryGetMappedMipmappedArray` and writes via surface
  objects.

Per-frame steady state: vendor lib writes directly into the
imported mipmapped array → signals timeline → no Vulkan-side blit
needed (the image is the consumer-visible resource). Trade-off:
no intermediate copy, but the vendor must natively support the
mipmapped-array sink and the format gate must hold.

### Hybrid (vendor writes flat, consumer wants mipmapped)

The streamlib NVIDIA reference flow (the `vulkanImageCUDA` sample)
combines both: vendor writes flat → CUDA-side
`cudaMemcpy2DToArrayAsync` into a Vulkan-imported mipmapped array.
Adds a CUDA copy per frame; not the recommended starting point.
Use only when buffer-flavored can't satisfy a tight texture-sampling
requirement.

## Capability tier

The engine primitives ([`HostVulkanBuffer::new_opaque_fd_export*`],
[`HostVulkanTexture::new_opaque_fd_export`],
[`HostVulkanTimelineSemaphore::new_exportable`]) are all
**FullAccess-only**. Backend construction (vendor SDK init, OPAQUE_FD
buffer allocation, timeline creation) happens at host-side
processor-setup time inside an `escalate(|full| ...)` closure. The
backend's steady-state per-frame work
(`vkCmdCopyBufferToImage`, timeline wait, ring rotation) runs on
LimitedAccess and never escalates.

Subprocess (Python / Deno cdylib) consumers reach the result via the
normal `surface_id` contract — they import the **ring slot's
render-target VkImage** (via surface-share, per
[`adapter-runtime-integration.md`](adapter-runtime-integration.md)),
not the internal OPAQUE_FD staging buffer. The vendor-library /
CUDA-side machinery stays inside the host-side backend; subprocess
customers consume the Vulkan output identically regardless of which
backend produced it.

If a subprocess customer needs to *invoke* a third-party GPU library
directly (Python-side `nvjpeg.decode(bytes)` returning a
subprocess-side handle), that's escalate-IPC shape — the subprocess
sends the request, the host runs the backend, the response carries
a `surface_id` the subprocess imports via the existing carve-out.
This is the same pattern every other escalate op already uses
(per [`subprocess-rhi-parity.md`](subprocess-rhi-parity.md)) and
doesn't require new engine surface.

## Probe + selection

Capability is exposed on [`HostVulkanDevice`] via a typed struct
([`ThirdPartyGpuCapabilities`]) probed once at device construction.
Each field is a `bool` named after the library
(`nvjpeg`, `optix_denoiser`, etc.); future siblings extend the
struct, not the device's method surface.

Selection inside a backend-using library (e.g. `SimpleJpegDecoder`)
is **runtime**: the library auto-selects the highest-tier backend
whose capability is `true`, with an optional caller override for
forcing a specific backend.

Build gating is **dynamic loading via `libloading`**, not a Cargo
feature. Backends that need a vendor `.so` (libnvjpeg, etc.) `dlopen`
it at probe time; build hosts without the library still compile
the backend code, and the probe simply returns `false` at runtime.
This matches the `cudarc` `fallback-dynamic-loading` shape already
used in the workspace and removes the "I forgot to enable the
Cargo feature" foot-gun.

## When to lift to engine-tier

**Today there is one shipped backend-using library** —
[`vulkan-jpeg`]'s `SimpleJpegDecoder`, which carries its own
`JpegDecodeBackend` trait with `VulkanComputeBackend` and
`NvJpegBackend` implementations. The shape is JPEG-specific by
design at this stage — a single consumer doesn't justify lifting
the trait to an engine-tier primitive yet.

**When a second backend-using library lands** (NVDEC-flavored
H.264/H.265 decoder, OptiX-denoiser post-processor, AMF encoder
backend swap), **do not copy the JPEG-specific trait shape**. The
trigger is to lift a generic `ThirdPartyGpuBackend` trait to the
engine layer, codifying:

- Backend identity + capability dependence (which
  `ThirdPartyGpuCapabilities` field gates it)
- The engine-allocated OPAQUE_FD staging surface contract
  (buffer-flavored or image-flavored)
- The timeline-semaphore signaling contract
- The Vulkan-side post-write step (`vkCmdCopyBufferToImage` for
  buffer-flavored; no-op for image-flavored)
- Runtime selection / override

The new trait lives next to [`ThirdPartyGpuCapabilities`] in the
engine's RHI module. Both the JPEG library and the second
backend-using library migrate to it in the same PR per CLAUDE.md
"No bad patterns left behind on engine changes." That migration
is the moment the engine-model lift happens — not before.

**The signal that you've hit the trigger** is that
[`ThirdPartyGpuCapabilities`] grew a second `bool` field. The struct
definition is the canonical place readers will look to understand
what backends exist; adding a field next to `nvjpeg` is the natural
prompt to read this doc and consider the lift.

## Anti-patterns

These are the failure modes the pattern exists to prevent. Each
matches a real foot-gun I've seen or one a future agent would
plausibly attempt without this doc.

1. **CUDA-allocates / Vulkan-imports.** Allocating via `cudaMalloc`
   + `cuMemExportToShareableHandle` and importing via
   `vkImportMemoryFdKHR` works for buffers but cannot bind to a
   tiled `VK_IMAGE_TILING_OPTIMAL` `VkImage` (the tile layout is
   opaque to CUDA's VMM allocator). It's also the inverse of every
   production pattern surveyed (Granite, Unreal NGX, Khronos
   OpenCL interop sample, NVIDIA `vulkanImageCUDA`). Stick with
   engine-allocates / vendor-imports.

2. **Subprocess-side vendor SDK init.** Linking a vendor SDK
   (libnvjpeg, libavcodec-CUDA, etc.) into the cdylib breaks the
   FullAccess vs LimitedAccess split — privileged allocation paths
   would re-emerge on the subprocess side. Vendor-SDK init stays
   on the host; subprocess customers reach the output via
   surface-share or escalate IPC.

3. **Parallel `JpegDecodeBackend`-shaped trait per library.** When
   the second backend-using library arrives, lift to engine-tier
   immediately (see "When to lift" above). Three hand-rolled
   JPEG/H.264/AMF backend traits is the failure mode this doc
   exists to prevent.

4. **Per-frame `vkAllocateMemory` on the OPAQUE_FD staging surface.**
   The staging buffer is allocated once at backend construction
   and reused across every frame — pre-warming the OPAQUE_FD pool
   (per [`docs/learnings/nvidia-opaque-fd-after-swapchain.md`](../learnings/nvidia-opaque-fd-after-swapchain.md))
   keeps the NVIDIA post-swapchain cap from biting. Allocating a
   fresh staging buffer per decode re-introduces the cap pressure
   the pre-warm machinery exists to avoid.

5. **Cargo-feature gating of the vendor SDK.** A Cargo feature
   means the build host has to remember to enable it. The cdylibs'
   `cudarc = { features = ["fallback-dynamic-loading"] }` is the
   reference shape — link nothing at build time, `dlopen` at
   runtime, fall back gracefully when the SDK isn't present.
   "I forgot to enable the feature flag for the drone-racer
   release" is the failure mode this rule prevents.

## Reference

- **Backend-using library**:
  [`sdk/vulkan-jpeg/src/backend.rs`](../../sdk/vulkan-jpeg/src/backend.rs)
  (`JpegDecodeBackend` trait),
  [`sdk/vulkan-jpeg/src/nvjpeg_backend.rs`](../../sdk/vulkan-jpeg/src/nvjpeg_backend.rs)
  (`NvJpegBackend` implementation).
- **Engine primitives**:
  - [`HostVulkanBuffer::new_opaque_fd_export`] (HOST_VISIBLE) and
    [`HostVulkanBuffer::new_opaque_fd_export_device_local`]
    (DEVICE_LOCAL) in
    `runtime/streamlib-engine/src/vulkan/rhi/vulkan_buffer.rs`.
  - [`HostVulkanTexture::new_opaque_fd_export`] in
    `runtime/streamlib-engine/src/vulkan/rhi/vulkan_texture.rs`.
  - [`HostVulkanTimelineSemaphore::new_exportable`] in
    `runtime/streamlib-engine/src/vulkan/rhi/vulkan_sync.rs`.
  - [`ThirdPartyGpuCapabilities`] on [`HostVulkanDevice`] in
    `runtime/streamlib-engine/src/vulkan/rhi/vulkan_device.rs`.
- **Companion docs**:
  - [`subprocess-rhi-parity.md`](subprocess-rhi-parity.md) —
    subprocess-side carve-out machinery (cdylib import shape).
  - [`adapter-runtime-integration.md`](adapter-runtime-integration.md) —
    how a subprocess obtains a surface adapter context.
  - [`texture-ring.md`](texture-ring.md) — ring shape backend
    libraries reuse for their render-target output slots.
  - [`compute-kernel.md`](compute-kernel.md) — the streamlib-
    native Vulkan-compute backend pattern (the alternative to
    routing through a vendor SDK).
- **Hard-won learnings**:
  - [`docs/learnings/nvidia-opaque-fd-after-swapchain.md`](../learnings/nvidia-opaque-fd-after-swapchain.md) —
    pre-warm sentinels keep post-swapchain OPAQUE_FD allocations
    from failing on NVIDIA Linux.
  - [`docs/learnings/cross-process-vkimage-layout.md`](../learnings/cross-process-vkimage-layout.md) —
    layout coordination for cross-process flows (relevant when a
    subprocess consumer reads the backend's output via
    surface-share).
- **External references**:
  - [NVIDIA cuda-samples `vulkanImageCUDA`](https://github.com/NVIDIA/cuda-samples/tree/master/cpp/5_Domain_Specific/vulkanImageCUDA)
    — canonical CUDA → Vulkan tiled-image interop.
  - [NVIDIA cuda-samples `simpleVulkanMMAP`](https://github.com/NVIDIA/cuda-samples/tree/master/cpp/5_Domain_Specific/simpleVulkanMMAP)
    — CUDA-side `cuMemExportToShareableHandle` plumbing (the
    inverse direction, anti-pattern #1 above).
  - [Granite `ExternalHandle`](https://github.com/Themaister/Granite/blob/master/vulkan/image.hpp) —
    closest precedent for a single-constructor engine-allocates /
    vendor-imports shape.
  - [`VK_KHR_external_memory_fd`](https://registry.khronos.org/vulkan/specs/latest/man/html/VK_KHR_external_memory_fd.html)
    — the Vulkan-side spec the host's OPAQUE_FD export rides.
