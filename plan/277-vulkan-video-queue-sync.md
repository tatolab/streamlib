---
whoami: amos
name: '@github:tatolab/streamlib#277'
adapters:
  github: builtin
description: vulkan-video synchronized queue submission via VulkanDevice — Route SimpleEncoder/SimpleDecoder queue_submit() calls through VulkanDevice's mutex-protected submit_to_queue() methods. Fixes release build SIGSEGV.
github_issue: 277
blocks:
- '@github:tatolab/streamlib#273'
---

@github:tatolab/streamlib#277

## Branch

Create `fix/vulkan-video-queue-sync` from `main` (after #273 merges).

## Steps

1. Modify `SimpleEncoder::from_device()` to accept `Arc<VulkanDevice>` alongside existing device/queue params
2. Modify `SimpleDecoder::from_device()` similarly
3. Replace `device.queue_submit()` in `encode/submit.rs` with `vulkan_device.submit_to_queue_legacy()`
4. Replace `device.queue_submit()` in `vk_video_decoder.rs` with `vulkan_device.submit_to_queue_legacy()`
5. Replace `device.queue_submit()` in `decode/mod.rs` (transfer queue) with `vulkan_device.submit_to_queue_legacy()`
6. Update H264/H265 encoder processors to pass `Arc<VulkanDevice>` from GpuContext
7. Update H264/H265 decoder processors to pass `Arc<VulkanDevice>` from GpuContext
8. Run existing encoder/decoder tests to confirm no regressions
