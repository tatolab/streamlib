---
whoami: amos
name: Retest camera + encoder + display roundtrip after Vulkan cleanup
status: pending
description: Rollup retest that supersedes #279. Run the full matrix after #287-#292, #296, #300, #302, #303, #304, #305, and #306 land and confirm release SIGSEGV and Cam Link OOM are both gone.
github_issue: 294
dependencies:
  - "down:Vulkanalia builder lifetime audit across RHI and processors"
  - "down:Camera ring textures missing TRANSFER_SRC_BIT"
  - "down:NV12 image views require VkSamplerYcbcrConversion"
  - "down:VMA bind-buffer-memory type mismatch"
  - "down:vkGetDeviceQueue called with unexposed family"
  - "down:Cam Link encoder ERROR_OUT_OF_DEVICE_MEMORY in debug"
  - "down:Display render_finished semaphore must be per-swapchain-image"
  - "down:Encoder src picture profile mismatch"
  - "down:Decoder pool probe uses hard-coded resolution"
  - "down:Camera MMAP path sees 0 frames on v4l2loopback"
  - "down:Flaky H.265 decoder DEVICE_LOST during setup"
  - "down:Fixture-based PSNR rig for encoder/decoder roundtrips"
  - "down:Expose encoder quality_level with real-time default"
adapters:
  github: builtin
---

@github:tatolab/streamlib#294

## Branch

Create `test/post-vulkan-cleanup-retest` from `main` after all dependencies merge.

## Steps

1. `cargo run --release -p vulkan-video-roundtrip -- h264 /dev/video0 30`
2. `cargo run --release -p vulkan-video-roundtrip -- h265 /dev/video0 30`
3. `cargo run --release -p vulkan-video-roundtrip -- h264 /dev/video2 30` (vivid)
4. Repeat 1-3 in debug.
5. Dynamic processor add/remove: start camera-only, then add encoder + display live.
6. Optional: run each scenario under `VK_LOADER_LAYERS_ENABLE="*validation*"` and confirm silence on the VUIDs targeted by #287-#291.

## Exit criteria

- All release runs: zero SIGSEGV, zero OOM, ≥ 25 fps through the pipeline for 30 s.
- Debug and release behaviour match.
- Dynamic add/remove succeeds without crashes.
