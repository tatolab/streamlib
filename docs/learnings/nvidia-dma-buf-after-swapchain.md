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
   linear and tiled, OPAQUE_FD HOST_VISIBLE and DEVICE_LOCAL buffers).
   The probe allocates a tiny resource through each pool and drops it;
   VMA retains the underlying `VkDeviceMemory` block for subsequent
   real allocations. Construction either yields a fully pre-warmed
   `Arc<HostVulkanDevice>` or fails — there is no half-formed instance
   for callers to observe. Companion learning for OPAQUE_FD:
   @docs/learnings/nvidia-opaque-fd-after-swapchain.md.

   > ~~**Pre-allocate exportable resources BEFORE creating the
   > swapchain.** Camera processors should acquire-and-release a
   > pixel buffer in their `start()` to trigger lazy pool creation
   > while the budget is freely available. See
   > `LinuxCameraProcessor::start()` for the exact pattern.~~ —
   > Superseded 2026-05-02 by engine-level pre-warm in
   > `HostVulkanDevice::new()` (issue #624). The consumer-level
   > pattern was load-bearing only because the engine deferred
   > block materialization to first use; once the engine pre-warms,
   > consumers no longer need to. The previous pattern persisted in
   > `camera.rs`, `display.rs`, `h264_decoder.rs`, and `h265_decoder.rs`
   > and was swept out in the same PR per the "no bad patterns left
   > behind on engine changes" rule in CLAUDE.md.

3. **Size per-frame Vulkan resources to MAX_FRAMES_IN_FLIGHT (2), not
   swapchain image_count.** See @docs/learnings/vulkan-frames-in-flight.md.

## References
- Bug fix: `cab6a00` `fix(vulkan): VMA pool isolation for DMA-BUF allocations`
- Refactor: `6816f54` `refactor(display): decouple frames-in-flight from swapchain image count`
- Engine pre-warm: issue #624, `fix(rhi): pre-warm export VMA pools at HostVulkanDevice construction`
- Repro test (does NOT trigger bug, documents attempt):
  `libs/streamlib/src/vulkan/rhi/vulkan_swapchain_alloc_repro_test.rs`
