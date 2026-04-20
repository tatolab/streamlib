---
whoami: amos
name: '@github:tatolab/streamlib#278'
adapters:
  github: builtin
description: Device-level lock for GPU resource creation during concurrent operations — Wrap vulkan-video session creation and DPB allocation with VulkanDevice::lock_device() to prevent races with concurrent GPU submissions.
github_issue: 278
blocks:
- '@github:tatolab/streamlib#277'
---

@github:tatolab/streamlib#278

## Branch

Create `fix/device-resource-creation-lock` from `main` (after #277 merges).

## Steps

1. Wrap `create_video_session_khr()` calls with `vulkan_device.lock_device()`
2. Wrap DPB image allocation (VMA `create_image`) with `lock_device()`
3. Wrap bitstream buffer allocation (VMA `create_buffer`) with `lock_device()`
4. Wrap video session memory binding (`bind_video_session_memory_khr`) with `lock_device()`
5. Verify encoder + decoder session setup doesn't deadlock with concurrent camera submissions
6. Run existing tests to confirm no regressions
