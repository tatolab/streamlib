---
whoami: amos
name: Retest camera + encoder + display roundtrip after Vulkan cleanup
status: completed
description: Rollup retest that supersedes #279. Run the full matrix after #287-#292, #296, #300, #302, #303, #304, #305, #306, #315, #316, and #330 land and confirm release SIGSEGV and Cam Link OOM are both gone.
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
  - "down:Enable samplerYcbcrConversion feature and audit NV12 image-create flags"
  - "down:Swapchain-descriptor image in UNDEFINED layout at sample time"
  - "down:Research: H.265 encoder quality configuration on Vulkan Video / NVIDIA"
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
6. Optional: run each scenario under `VK_LOADER_LAYERS_ENABLE="*validation*"` and confirm silence on the VUIDs targeted by #287-#291, #296, #300, #315, #316.

## Exit criteria

- All release runs: zero SIGSEGV, zero OOM, ≥ 25 fps through the pipeline for 30 s.
- Debug and release behaviour match.
- Dynamic add/remove succeeds without crashes.

## Outcome

Completed 2026-04-19 on branch `test/post-vulkan-cleanup-retest`.
Full report: [docs/retests/294-post-vulkan-cleanup-retest.md](../docs/retests/294-post-vulkan-cleanup-retest.md).

Overall stability score: **70 / 100 — conditional go.** First-pass
matrix flagged 8 issues; cold-start re-verification demoted 4 to
harness non-hermeticity false positives, leaving 2 real blockers and
1 newly-surfaced harness gap.

Follow-ups filed:

- #335 — P0 H.265 shutdown race when decoder lags encoder
- #336 — P1 NV12 direct-ingest chroma collapse (RGBA↔NV12 converter)
- #337 — P1 E2E test harness non-hermetic on NVIDIA Linux
- #338 — P2 `StreamRuntime` does not honor validation-layer env vars
- #339 — P3 vivid pipeline throughput ≈ 5 fps
- #340 — P3 No example for dynamic processor add/remove

Next retest queued as #341, gated on #335/#336/#337/#338 and the
#319 GPU capability umbrella.
