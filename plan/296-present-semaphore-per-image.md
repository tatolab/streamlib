---
whoami: amos
name: Display render_finished semaphore must be per-swapchain-image
status: completed
description: Size render_finished_semaphores to swapchain image_count and index by image_index, not MAX_FRAMES_IN_FLIGHT / frame_index, so the present engine's hold on the binary semaphore doesn't collide with the next signal.
github_issue: 296
adapters:
  github: builtin
---

@github:tatolab/streamlib#296

## Branch

Create `fix/present-semaphore-per-image` from `main`.

## Steps

1. In `libs/streamlib/src/linux/processors/display.rs`:
   - Change `render_finished_semaphores` allocation to size `swapchain_images.len()` (create path ~1194–1205 and recreate path ~1594–1615).
   - `image_available_semaphores` stays at `MAX_FRAMES_IN_FLIGHT` (per-frame-in-flight CPU↔GPU sync).
   - At submit (~640) / present (~894): index `render_finished_semaphores` by `image_index`, not `frame_index`.
2. Update `docs/learnings/vulkan-frames-in-flight.md` table: render-finished / present-wait semaphore is per-swapchain-image, not per-frame-in-flight. Note the present-engine-holds-it-until-release rationale.
3. Re-run release E2E under `VK_INSTANCE_LAYERS=VK_LAYER_KHRONOS_validation`; confirm `VUID-vkQueueSubmit2-semaphore-03868` is silent.

## Verification

- Validation layer silent on `VUID-vkQueueSubmit2-semaphore-03868` during steady-state.
- E2E fixture still passes: clean shutdown, zero OOM, ≥ 25 fps.
- No visual regressions.
