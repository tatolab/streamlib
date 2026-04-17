---
whoami: amos
name: VulkanDevice thread-safe queue submission synchronization
status: pending
description: Add per-queue mutexes to VulkanDevice so concurrent processor threads can safely submit Vulkan commands. Fixes release build SIGSEGV.
github_issue: 273
dependencies:
  - "down:@github:tatolab/streamlib#270"
adapters:
  github: builtin
---

@github:tatolab/streamlib#273

## Branch

Create `fix/vulkan-device-synchronization` from `main` (after #270 merges).

## Steps

1. Add per-queue `Mutex<()>` fields to `VulkanDevice` (one per queue family)
2. Create `VulkanDevice::submit_to_queue(&self, queue: vk::Queue, ...)` that acquires the appropriate lock
3. Add a broader device-level lock for resource creation (video sessions, VMA allocations)
4. Update camera capture thread to use the synchronized submission path
5. Update encoder/decoder processors to use the synchronized path
6. Verify release build: camera + encoder pipeline runs without SIGSEGV
7. Verify dynamic processor add/remove doesn't crash
8. Run PSNR integration tests to confirm no quality regression
