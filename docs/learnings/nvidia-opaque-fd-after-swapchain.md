# NVIDIA Linux: OPAQUE_FD allocations are capped after swapchain creation

## Symptom

`VK_ERROR_OUT_OF_DEVICE_MEMORY` returned from `vmaCreateBuffer` (or
the host RHI's `HostVulkanPixelBuffer::new_opaque_fd_export*`) when:

- Running on NVIDIA Linux Vulkan driver
- A `VkSwapchainKHR` has been created in this process
- The allocation chains `VkExportMemoryAllocateInfo::handleTypes =
  OPAQUE_FD` (the export type CUDA / OpenCL `cudaImportExternalMemory`
  expects)

Error message looks like real OOM but is NOT — the device has plenty
of free VRAM. The driver is enforcing a quota on OPAQUE_FD-exportable
memory, just as it does for DMA-BUF (see
@docs/learnings/nvidia-dma-buf-after-swapchain.md). Different export
handle type, same kernel-side budget pressure.

**Failure pattern observed in streamlib:**
`CameraToCudaCopyProcessor::setup_inner` (in
`examples/camera-python-display`) called
`HostVulkanPixelBuffer::new_opaque_fd_export_device_local` after the
display processor's render thread had created its swapchain, and the
allocation failed with `A device memory allocation has failed`. Issue
#624.

## Root cause

NVIDIA's Linux driver tracks per-process exportable-memory budget per
handle-type (DMA-BUF and OPAQUE_FD count separately, but both are
budgeted). The Wayland/X11 compositor imports the swapchain images
as DMA-BUFs to display them — that consumes the DMA-BUF budget. The
swapchain itself reserves additional kernel resources that affect
fresh `vkAllocateMemory` calls for any other exportable handle type,
including OPAQUE_FD.

VMA pools defer their underlying `VkDeviceMemory` block to first use:
`vmaCreatePool` reserves no kernel memory; the first
`vmaCreate{Buffer,Image}` from the pool issues a real
`vkAllocateMemory` to back the block. If that first allocation lands
post-swapchain, NVIDIA's exportable budget is already spoken for and
the call fails.

The fix shape is identical to the DMA-BUF case: materialize the pool's
backing block while the budget is freely available, then sub-allocate
from it.

## Fix

The engine handles this. `HostVulkanDevice::new()` pre-warms every
export-capable VMA pool — DMA-BUF buffer, DMA-BUF image linear,
DMA-BUF image tiled, OPAQUE_FD HOST_VISIBLE buffer, OPAQUE_FD
DEVICE_LOCAL buffer — by allocating a small probe through each one
and dropping it. VMA retains the block; subsequent real allocations
from any consumer sub-allocate from it.

`new()` returns `Result<Arc<Self>>` so the pre-warm step can call
back through the public RHI constructors (which take
`&Arc<HostVulkanDevice>`). This is the production pattern (Unreal
`RHICreateDevice`, Bevy renderer init, wgpu-core `request_device`):
construction either yields a fully-usable instance with all
init-time invariants run, or fails — there is no half-formed state
observable to callers.

**Consumers do NOT need to pre-warm.** If you find yourself wanting
to allocate-and-drop an exportable resource at processor `start()`
time, you're re-deriving the dead pattern; the engine already did it
before any of your code ran. See CLAUDE.md's "Engine-wide bugs get
fixed at the engine layer" and "No bad patterns left behind on engine
changes" rules.

## When the constraint still bites

The engine pre-warm sizes blocks via VMA's default heuristic. If a
consumer needs a single exportable allocation larger than the block
VMA picked (typically up to ~256 MiB on heaps ≥ 1 GiB), VMA may
attempt a fresh `vkAllocateMemory` for a new block — which can hit
the cap. For typical workloads (1080p / 4K / 8K BGRA buffers, ≤ ~133
MiB) this stays within one block.

If a future consumer needs allocations beyond a single block:
- Don't reach for a consumer-level pre-warm — that's the dead pattern.
- Extend the engine to set explicit `VmaPoolCreateInfo::blockSize` on
  the affected pool, or add a typed "pre-allocate this consumer's
  worst-case footprint" hook the engine runs at construction.
- Surface the constraint to the user — block-size tuning is an
  engine-shape decision, not a per-processor workaround.

## Reference

- Bug fix: issue #624, `fix(rhi): pre-warm export VMA pools at HostVulkanDevice construction`
- Sibling learning: @docs/learnings/nvidia-dma-buf-after-swapchain.md
- VMA pool pattern: @docs/learnings/vma-export-pools.md
