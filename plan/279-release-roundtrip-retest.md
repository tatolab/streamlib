---
whoami: amos
name: Retest camera + encoder + display roundtrip in release build
status: pending
description: Validate full GPU pipeline (camera + encoder + decoder + display) in release build after synchronization fixes land. Confirms no SIGSEGV or OOM.
github_issue: 279
dependencies:
  - "down:vulkan-video synchronized queue submission via VulkanDevice"
  - "down:Device-level lock for GPU resource creation during concurrent operations"
adapters:
  github: builtin
---

@github:tatolab/streamlib#279

## Branch

Create `test/release-roundtrip-retest` from `main` (after #277 + #278 merge).

## Steps

1. H.264 roundtrip with Cam Link 4K: `cargo run --release -p vulkan-video-roundtrip -- h264 /dev/video0 30`
2. H.265 roundtrip with Cam Link 4K: `cargo run --release -p vulkan-video-roundtrip -- h265 /dev/video0 30`
3. Vivid virtual camera roundtrip: `cargo run --release -p vulkan-video-roundtrip -- h264 /dev/video10 30`
4. Dynamic processor add/remove: start camera-only, add encoder while running
5. Release vs debug parity: run all above in both modes
6. Document results and close retest items from #273 and #272/PR #275
