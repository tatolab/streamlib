---
whoami: amos
name: Swapchain-descriptor image in UNDEFINED layout at sample time
status: pending
description: Fix the display render submit path so every sampled/storage image bound via descriptor is in the layout the descriptor was written for, silencing VUID-vkCmdDraw-None-09600 (sees UNDEFINED, expects PRESENT_SRC_KHR).
github_issue: 316
adapters:
  github: builtin
---

@github:tatolab/streamlib#316

## Branch

Create `fix/swapchain-descriptor-undefined-layout` from `main`.

## Steps

1. Reproduce under `VK_LOADER_LAYERS_ENABLE=*validation*` with `vulkan-video-roundtrip h264 /dev/video2 15` and capture the exact `VkImage` handle reported by `VUID-vkCmdDraw-None-09600`.
2. Cross-reference the handle against:
   - `state.swapchain_images` (swapchain side — UNDEFINED only until the first pre-render barrier fires)
   - The camera ring textures (sampled by the fragment shader, expected in `SHADER_READ_ONLY_OPTIMAL`)
3. Whichever image it is, fix the mismatch:
   - If it's a swapchain image: confirm the pre-render `UNDEFINED → COLOR_ATTACHMENT_OPTIMAL` barrier still fires before any command that samples the image, and the post-render `→ PRESENT_SRC_KHR` barrier still fires before present. Validate ordering around the per-image `render_finished_semaphore` change in #296.
   - If it's a camera texture: confirm the descriptor binding uses the layout the camera-side barrier left it in, not `PRESENT_SRC_KHR`. Likely a stale layout in the image info struct passed to `VkWriteDescriptorSet`.
4. Re-run with validation layer on and confirm `-09600` is silent across the entire run (not just steady state — the current occurrences are clustered in the first few frames).

## Verification

- `VK_LOADER_LAYERS_ENABLE=*validation*` `vulkan-video-roundtrip h264 /dev/video2 15` 300-frame run: zero instances of `VUID-vkCmdDraw-None-09600`.
- Same for h265 and for `camera-display`.
- No visual regressions in the PNG samples.

## Context

Discovered during #296 E2E validation sweep. Baseline `main` has 8 occurrences per 300-frame run, patched #296 build still has 8 — the VUID is orthogonal to the `render_finished_semaphore` fix. Worth confirming the count does not increase after #296 lands.
