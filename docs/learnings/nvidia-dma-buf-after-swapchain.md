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

Don't waste time trying to reproduce in pure unit tests. Validate the fix
end-to-end via @docs/learnings/camera-display-e2e-validation.md.

## Fix

1. **Don't set VMA `pTypeExternalMemoryHandleTypes` globally.** Use VMA
   custom pools with `pMemoryAllocateNext` for the specific allocations
   that need DMA-BUF export. See @docs/learnings/vma-export-pools.md.

2. **The engine pre-warms every export-capable VMA pool at
   `HostVulkanDevice::new()` time** (DMA-BUF buffers, DMA-BUF images
   linear and tiled, OPAQUE_FD HOST_VISIBLE and DEVICE_LOCAL buffers)
   by allocating a tiny probe through each pool, strictly before any
   caller can build a `VkSwapchainKHR`. Empirical observation from
   issue #624: this keeps the post-swapchain allocation path open for
   that handle type. DMA-BUF probes are dropped. Note that the host RHI
   pixel-buffer and texture constructors all set
   `vma::AllocationCreateFlags::DEDICATED_MEMORY`, so every export
   allocation is its own `VkDeviceMemory` and dropping the probe
   actually issues `vkFreeMemory` — VMA does not "retain a block"
   for subsequent allocations. The cap-bypass mechanism is internal
   to NVIDIA's driver rather than VMA-side block retention: the
   per-handle-type state stays open while a live allocation of that
   type anchors it, and for DMA-BUF the compositor's swapchain imports
   are that live allocation. OPAQUE_FD has no ambient anchor, so its
   probes are retained as long-lived sentinels. Construction either
   yields a fully pre-warmed `Arc<HostVulkanDevice>` or fails — there
   is no half-formed instance for callers to observe. Companion
   learning for OPAQUE_FD: @docs/learnings/nvidia-opaque-fd-after-swapchain.md.

3. **Size per-frame Vulkan resources to MAX_FRAMES_IN_FLIGHT (2), not
   swapchain image_count.** See @docs/learnings/vulkan-frames-in-flight.md.

## References
- Bug fix: `cab6a00` `fix(vulkan): VMA pool isolation for DMA-BUF allocations`
- Refactor: `6816f54` `refactor(display): decouple frames-in-flight from swapchain image count`
- Engine pre-warm: issue #624, `fix(rhi): pre-warm export VMA pools at HostVulkanDevice construction`
- Fix validation behind a live swapchain — clean VMA config, export pools:
  `runtime/streamlib-engine/src/vulkan/rhi/vulkan_swapchain_dma_buf_allocation_fix_validation_test.rs`
