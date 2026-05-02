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

## Root cause (empirical)

NVIDIA's Linux driver tracks per-process exportable-memory state per
handle-type (DMA-BUF and OPAQUE_FD are budgeted separately, both are
affected). The Wayland/X11 compositor imports the swapchain images
as DMA-BUFs to display them — that consumes the DMA-BUF state. The
swapchain itself reserves additional kernel resources that affect
fresh `vkAllocateMemory` calls for any other exportable handle type,
including OPAQUE_FD: after `vkCreateSwapchainKHR`, the *first* call
to `vkAllocateMemory` for a handle type that hasn't been allocated
yet in this process returns `VK_ERROR_OUT_OF_DEVICE_MEMORY`.

The exact mechanism is internal to NVIDIA's driver. What's
empirically observable: a successful `vkAllocateMemory` for the
handle type *before* `vkCreateSwapchainKHR` is sufficient to keep
the post-swapchain allocation path open, even after that allocation
is freed. The reservation appears to be one-way — first allocation
initializes some kernel-side state that survives the free.

This is **not** about VMA block retention. The host RHI's pixel-
buffer and texture constructors all set
`vma::AllocationCreateFlags::DEDICATED_MEMORY`, so each export
allocation is its own `VkDeviceMemory`; dropping the probe issues a
real `vkFreeMemory`. The engine pre-warm works in spite of that, not
because of it — what survives the free is on NVIDIA's side, not on
VMA's.

## Fix

`HostVulkanDevice::new()` pre-warms every export-capable VMA pool —
DMA-BUF buffer, DMA-BUF image linear, DMA-BUF image tiled, OPAQUE_FD
HOST_VISIBLE buffer, OPAQUE_FD DEVICE_LOCAL buffer — by allocating
a small probe through each one and dropping it, strictly before any
caller can build a `VkSwapchainKHR`. The probes share the standard
host RHI constructors (so they trigger the same DEDICATED_MEMORY
allocation codepath that real consumers will hit), guaranteeing that
whatever NVIDIA-side state needs initializing has been initialized.

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

## Verifying / re-deriving the fix

When in doubt about whether the engine pre-warm still works (driver
update, VMA refactor, new export handle type, etc.), the
deterministic protocol is:

1. Edit `vulkan_device.rs` to comment out the
   `Self::prewarm_export_pools(&device)?;` call in
   `HostVulkanDevice::new()`.
2. `cargo build --release -p camera-python-display`.
3. Run with `STREAMLIB_CAMERA_DEVICE=/dev/video2`,
   `STREAMLIB_DISPLAY_FRAME_LIMIT=180`, `timeout --kill-after=5 30`.
4. Without the pre-warm: log shows `Setup failed: ... CameraToCudaCopy:
   new_opaque_fd_export_device_local: ... A device memory allocation
   has failed.`
5. Re-enable, rebuild, re-run: log shows `HostVulkanDevice export
   pools pre-warmed` followed by `CameraToCudaCopy: registered cuda
   OPAQUE_FD DEVICE_LOCAL surface_id=...` — no setup failure.

If step 4 stops reproducing the failure on a fresh driver, the
NVIDIA-side mechanism may have changed and this learning is stale.
Update accordingly.

## Reference

- Bug fix: issue #624, `fix(rhi): pre-warm export VMA pools at HostVulkanDevice construction`
- Sibling learning: @docs/learnings/nvidia-dma-buf-after-swapchain.md
- VMA pool pattern: @docs/learnings/vma-export-pools.md
- Empirical verification protocol above documents the reproducer
  (since the bug doesn't trigger in isolated unit tests, per the
  DMA-BUF learning).
