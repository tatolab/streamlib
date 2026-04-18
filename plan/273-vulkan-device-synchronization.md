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

## Retest after fix (discovered in #272 / PR #275)

The following issues were hit during roundtrip verification and are caused by
unsynchronized concurrent GPU resource creation. Retest all of these after
the synchronization fix lands:

1. **Cam Link 4K + display + roundtrip**: encoder `encode_image()` fails with
   `ERROR_OUT_OF_DEVICE_MEMORY` after swapchain creation. The total DMA-BUF
   resource count (encoder DPB + decoder DPB + camera textures + swapchain)
   exceeds NVIDIA's budget when resources race. Serialized creation should
   fit within the budget.
   - Test: `cargo run -p vulkan-video-roundtrip -- h264 /dev/video0 30`
   - Test: `cargo run -p vulkan-video-roundtrip -- h265 /dev/video0 30`
   - Expected: display window shows decoded camera feed, no OOM errors

2. **Resource creation ordering**: encoder + decoder video sessions + DPB
   must be created BEFORE display swapchain. Current workarounds (eager
   encoder init, decoder `pre_initialize_session()`) partially fix this
   but don't solve the concurrent allocation race.

3. **Release build SIGSEGV**: original issue — camera GPU compute dispatch
   concurrent with encoder video session setup crashes NVIDIA driver.

4. **Vivid full/limited range color**: separate from #273 but should be
   verified doesn't regress — vivid outputs BT.601 full range, encoder
   uses BT.709 narrow range. Green tint is expected until color space
   handling is unified (separate task).
