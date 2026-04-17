---
whoami: amos
name: GPU-Resident VkImage Pipeline
status: in_review
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

## What this solves

The current pipeline has a GPU→CPU→GPU roundtrip every frame:

```
Camera:  compute shader → DEVICE_LOCAL image → cmd_copy_image_to_buffer → HOST_VISIBLE pixel buffer
Display: HOST_VISIBLE pixel buffer → cmd_copy_buffer_to_image → DEVICE_LOCAL texture → fragment shader
```

At 1920x1080 BGRA = 8.3 MB/frame × 2 PCIe crossings × 30fps ≈ 500 MB/s wasted. This task eliminates the HOST_VISIBLE intermediary for same-process pipelines.

## Changes

Rebuilt clean with vulkanalia (NOT cherry-picked from old branches). The patterns come from commit `dcba5cf` on `feat/233-ffmpeg-vulkan-codecs` but the code will use vulkanalia types.

### gpu_context.rs
- Add `texture_cache: Arc<Mutex<HashMap<String, StreamTexture>>>`
- `register_texture(id, texture)` — camera registers output textures
- `resolve_videoframe_texture(frame)` — encoder/display looks up by surface_id
- `acquire_output_texture(w, h, format)` — decoder acquires output texture with new UUID

### camera.rs
- **2-texture ring** (DEVICE_LOCAL, STORAGE | SAMPLED usage flags)
- Compute shader writes **directly** to ring texture (eliminates `compute_output_image` intermediary)
- Remove `cmd_copy_image_to_buffer` (no GPU→CPU→GPU roundtrip)
- Replace CPU `wait_for_fences` with timeline semaphore signal — publish `(surface_id, semaphore, value)` in Videoframe
- 4MB thread stack for large IPC payloads

### display.rs
- Direct texture sampling from surface_id via `resolve_videoframe_texture()`
- Remove `camera_texture_ring` allocation and `cmd_copy_buffer_to_image` upload
- Wait on camera's timeline semaphore at `FRAGMENT_SHADER` stage before sampling
- Queue lock for concurrent submissions
- PNG sampling: one-off GPU readback to staging buffer when env var set (not in hot path)

### vulkan_texture.rs
- Lazy-cached image_view() via OnceLock

### Shaders
- Rename nv12_to_bgra → nv12_to_rgba, yuyv_to_bgra → yuyv_to_rgba
- Fix channel ordering (RGBA not BGRA)

## Hard-won learnings — MUST follow

These constraints come from validated discoveries on the ash-to-vulkanalia branch. Violating them causes silent failures or OOM on NVIDIA.

### Ring size = 2, NOT 4

The original spec said 4-texture ring. This is wrong:

- `docs/learnings/vulkan-frames-in-flight.md`: per-frame resources = `MAX_FRAMES_IN_FLIGHT = 2`. Display only has 2 frames in flight — producing 4 ahead is pointless.
- `docs/learnings/nvidia-dma-buf-after-swapchain.md`: NVIDIA allows ~2 DMA-BUF exportable allocations after swapchain creation. 4 exportable textures will OOM.
- `docs/learnings/vma-export-pools.md`: exportable textures must use isolated DMA-BUF image pool.

Camera ring textures use `STORAGE | SAMPLED` flags. For same-process they do NOT need DMA-BUF export → use default VMA pool (no export flags) → dodges NVIDIA cap entirely.

### Eliminate compute_output_image

Current camera.rs has: SSBO → compute shader → compute_output_image → cmd_copy_image_to_buffer → pixel buffer. With this change: SSBO → compute shader → ring_texture directly. One less allocation, one less GPU-side copy. The ring texture needs `STORAGE | SAMPLED` usage (not just `TRANSFER_DST | SAMPLED` like the current display camera textures).

### Pre-allocation timing

Camera `start()` runs before display creates the swapchain. Allocate ring textures in camera `start()` to get the NVIDIA budget before swapchain consumes it. This matches the existing pattern in `LinuxCameraProcessor::start()` that pre-acquires a pixel buffer.

### Same-process only (for now)

The texture_cache approach is same-process only (camera + display share VulkanDevice). Cross-process keeps the HOST_VISIBLE pixel buffer path via SurfaceStore/broker. Cross-process GPU-native sharing can follow as a separate task.

## Vulkan 1.4 — use new APIs

The codebase targets Vulkan 1.4. Use the core APIs, not the old compatibility paths:

### Use `cmd_pipeline_barrier_2` (sync2) for ALL barriers

```rust
// OLD (do NOT use) — shared stage masks, empty-slice Cast workaround
device.cmd_pipeline_barrier(cmd, src_stage, dst_stage, deps,
    &[] as &[vk::MemoryBarrier],        // Cast workaround
    &[] as &[vk::BufferMemoryBarrier],   // Cast workaround
    &[image_barrier]);

// NEW — per-barrier stages, no empty slice issue
let barrier = vk::ImageMemoryBarrier2::builder()
    .src_stage_mask(vk::PipelineStageFlags2::COMPUTE_SHADER)
    .src_access_mask(vk::AccessFlags2::SHADER_WRITE)
    .dst_stage_mask(vk::PipelineStageFlags2::FRAGMENT_SHADER)
    .dst_access_mask(vk::AccessFlags2::SHADER_READ)
    .old_layout(vk::ImageLayout::GENERAL)
    .new_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)
    .image(ring_texture)
    .subresource_range(range)
    .build();
let dep_info = vk::DependencyInfo::builder()
    .image_memory_barriers(&[barrier])
    .build();
device.cmd_pipeline_barrier_2(cmd, &dep_info);
```

This eliminates the `&[] as &[vk::MemoryBarrier]` Cast workaround documented in `docs/learnings/vulkanalia-empty-slice-cast.md`. Use `PipelineStageFlags2::NONE` instead of `TOP_OF_PIPE`/`BOTTOM_OF_PIPE`.

### Use `queue_submit_2` for ALL submits

```rust
// OLD (do NOT use) — parallel arrays, magic 0 for binary, pNext shim
let mut timeline_submit_info = vk::TimelineSemaphoreSubmitInfo::builder()...;
let submit = vk::SubmitInfo::builder()
    .push_next(&mut timeline_submit_info)...;
device.queue_submit(queue, &[submit], fence)?;

// NEW — each semaphore is self-contained
let wait = vk::SemaphoreSubmitInfo::builder()
    .semaphore(image_available)
    .stage_mask(vk::PipelineStageFlags2::COLOR_ATTACHMENT_OUTPUT)
    .build();
let signal_timeline = vk::SemaphoreSubmitInfo::builder()
    .semaphore(frame_timeline)
    .value(timeline_value)
    .stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)
    .build();
let cmd_info = vk::CommandBufferSubmitInfo::builder()
    .command_buffer(cmd)
    .build();
let submit = vk::SubmitInfo2::builder()
    .wait_semaphore_infos(&[wait])
    .signal_semaphore_infos(&[signal_timeline])
    .command_buffer_infos(&[cmd_info])
    .build();
device.queue_submit_2(queue, &[submit], vk::Fence::null())?;
```

No parallel array alignment bugs, no `TimelineSemaphoreSubmitInfo` pNext shim, per-semaphore stage mask.

### Remove redundant extension loading

These are core in 1.4 — remove from `device_extensions` in vulkan_device.rs:
- `VK_KHR_dynamic_rendering` (core since 1.3)
- `VK_KHR_synchronization2` (core since 1.3)

The feature enable structs (`PhysicalDeviceDynamicRenderingFeatures`, `PhysicalDeviceSynchronization2Features`) are still needed — just not the extension strings.

## Testing goals

- Camera outputs DEVICE_LOCAL textures, no HOST_VISIBLE pixel buffer in the video path
- Display samples camera textures directly, no `cmd_copy_buffer_to_image`
- Timeline semaphore handoff between camera and display (no CPU fence wait)
- Texture ring size = 2, verified by log output
- All barriers use `cmd_pipeline_barrier_2` (sync2)
- All submits use `queue_submit_2`
- E2E validation via `e2e_camera_display.sh` (depends on #236 clean exit)
- No regression: DMA-BUF zero-copy V4L2 import path still works
- No regression: MMAP fallback path still works
