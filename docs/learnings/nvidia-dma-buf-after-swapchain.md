# NVIDIA Linux: DMA-BUF allocations are capped after swapchain creation

## Symptom

`VK_ERROR_OUT_OF_DEVICE_MEMORY` returned from `vmaCreateBuffer`,
`vmaCreateImage`, or raw `vkAllocateMemory` when:

- Running on NVIDIA Linux Vulkan driver
- A `VkSwapchainKHR` has been created in this process
- The allocation chains `VkExportMemoryAllocateInfo` with `DMA_BUF_EXT`
  handle type (set explicitly OR via VMA's
  `VmaAllocatorCreateInfo::pTypeExternalMemoryHandleTypes`)

Error message looks like real OOM but is NOT — the device has plenty of
free VRAM. The driver is enforcing a quota on DMA-BUF exportable memory.

**Failure pattern observed in streamlib:** display processor's
`vmaCreateImage` for the camera texture ring fails on the 3rd allocation
attempt (textures [0] and [1] succeed, [2] fails immediately). Repeats on
every frame, never recovers.

## Root cause

The Wayland/X11 compositor imports the swapchain images as DMA-BUFs to
display them. This consumes part of NVIDIA's per-process DMA-BUF
allocation budget. After the swapchain is bound, the budget is largely
spoken for, and only ~2 more new exportable `VkDeviceMemory` allocations
can be created before the driver returns OOM.

This is invisible if VMA is configured correctly (each block holds many
sub-allocations) but becomes catastrophic when:
- VMA's global `pTypeExternalMemoryHandleTypes` makes EVERY block exportable
- OR you use `DEDICATED_MEMORY` flag for many allocations (each is its own block)

## The bug doesn't reproduce in isolated unit tests

Even with: visible window + active swapchain + same allocation pattern +
the broken VMA config, isolated unit tests do NOT reproduce the failure.
The bug needs production-level GPU work happening concurrently and live
compositor DMA-BUF imports — not just a swapchain in idle state.

Don't waste time trying to reproduce in pure unit tests. Validate a fix
end-to-end on a machine with a live compositor importing DMA-BUFs.

## Fix

1. **Don't set VMA `pTypeExternalMemoryHandleTypes` globally.** Use VMA
   custom pools with `pMemoryAllocateNext` for the specific allocations
   that need DMA-BUF export. See @docs/learnings/vma-export-pools.md.

2. **The engine anchors every export-capable VMA pool at
   `HostVulkanDevice::new()` time** (DMA-BUF buffers, DMA-BUF images
   linear and tiled, OPAQUE_FD HOST_VISIBLE, host-cached and DEVICE_LOCAL
   buffers, and the OPAQUE_FD image pool)
   by allocating a tiny (256-byte) sentinel out of each pool,
   strictly before any caller can build a `VkSwapchainKHR`, and
   holding every sentinel for the device's life. Empirical
   observation from issue #624: an allocation of the handle type
   before the swapchain keeps the post-swapchain allocation path open.
   Note that the host RHI pixel-buffer and texture constructors all
   set `vma::AllocationCreateFlags::DEDICATED_MEMORY`, so every export
   allocation is its own `VkDeviceMemory` and freeing one issues
   `vkFreeMemory` — VMA does not "retain a block" for subsequent
   allocations. The cap-bypass mechanism is internal to NVIDIA's
   driver rather than VMA-side block retention: the per-handle-type
   state stays open while a live allocation of that type anchors it.
   The shared frame pools free every frame of a flavour once it goes
   unused, so the engine keeps its own anchor per pool rather than
   resting on the compositor's swapchain imports. Construction either
   yields an `Arc<HostVulkanDevice>` holding every sentinel or fails —
   there is no half-formed instance for callers to observe. Companion
   learning for OPAQUE_FD: @docs/learnings/nvidia-opaque-fd-after-swapchain.md.

3. **Size per-frame Vulkan resources to MAX_FRAMES_IN_FLIGHT (2), not
   swapchain image_count.** See @docs/learnings/vulkan-frames-in-flight.md.

## References
- Bug fix: `cab6a00` `fix(vulkan): VMA pool isolation for DMA-BUF allocations`
- Refactor: `6816f54` `refactor(display): decouple frames-in-flight from swapchain image count`
- Engine pre-warm: issue #624, `fix(rhi): pre-warm export VMA pools at HostVulkanDevice construction`
