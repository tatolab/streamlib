---
whoami: amos
name: GPU-Resident VkImage Pipeline
status: pending
description: Rebuild camera texture ring, display direct sampling, and texture cache with vulkanalia types. Branch feat/gpu-resident-pipeline from main.
github_issue: 253
dependencies:
  - "down:@github:tatolab/streamlib#236"
adapters:
  github: builtin
---

@github:tatolab/streamlib#253

## Branch

Create `feat/gpu-resident-pipeline` from `main` (after #236 merges).

## Changes

Rebuilt clean with vulkanalia (NOT cherry-picked from old branches). The patterns come from commit `dcba5cf` on `feat/233-ffmpeg-vulkan-codecs` but the code will use vulkanalia types.

### gpu_context.rs
- Add `texture_cache: Arc<Mutex<HashMap<String, StreamTexture>>>`
- `register_texture(id, texture)` — camera registers output textures
- `resolve_videoframe_texture(frame)` — encoder/display looks up by surface_id
- `acquire_output_texture(w, h, format)` — decoder acquires output texture with new UUID

### camera.rs
- 4-texture ring (DEVICE_LOCAL, not HOST_VISIBLE)
- Remove vkCmdCopyImageToBuffer (no GPU→CPU→GPU roundtrip)
- 4MB thread stack for large IPC payloads

### display.rs
- Direct texture sampling from surface_id (remove camera_texture_ring + buffer upload)
- Queue lock for concurrent submissions

### vulkan_texture.rs
- Lazy-cached image_view() via OnceLock

### Shaders
- Rename nv12_to_bgra → nv12_to_rgba, yuyv_to_bgra → yuyv_to_rgba
- Fix channel ordering (RGBA not BGRA)
