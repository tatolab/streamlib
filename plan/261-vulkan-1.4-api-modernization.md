---
whoami: amos
name: Vulkan 1.4 API Modernization
status: completed
description: Migrate all Vulkan API usage to 1.4 core — sync2 barriers, queue_submit_2, extension cleanup. Performance gain + code simplification.
github_issue: 261
dependencies:
  - "down:@github:tatolab/streamlib#254"
adapters:
  github: builtin
---

@github:tatolab/streamlib#261

## Branch

Create `refactor/vulkan-1.4-api-modernization` from `main` (after #253 merges).

## Context

The codebase targets Vulkan 1.4 and already enables sync2/timeline/dynamic-rendering features, but all API calls use the old 1.0/1.1 patterns. #253 introduces new code using 1.4 APIs (sync2 barriers, queue_submit_2). This task catches everything #253 doesn't touch.

## Scope

### 1. Barriers: `cmd_pipeline_barrier` → `cmd_pipeline_barrier_2`

Migrate every `cmd_pipeline_barrier` call to use `DependencyInfo` + `ImageMemoryBarrier2` / `BufferMemoryBarrier2`:
- `PipelineStageFlags` → `PipelineStageFlags2`
- `AccessFlags` → `AccessFlags2`
- `TOP_OF_PIPE`/`BOTTOM_OF_PIPE` → `PipelineStageFlags2::NONE`
- Per-barrier stage+access instead of shared stage masks

Files to check:
- `vulkan/rhi/vulkan_format_converter.rs`
- `vulkan/rhi/vulkan_blitter.rs`
- Any remaining calls in `camera.rs` / `display.rs` that #253 didn't touch
- Any other file using `cmd_pipeline_barrier`

Eliminates the `&[] as &[vk::MemoryBarrier]` Cast workaround (vulkanalia-empty-slice-cast.md).

### 2. Submits: `queue_submit` → `queue_submit_2`

Migrate every `queue_submit` call to `SubmitInfo2` + `SemaphoreSubmitInfo` + `CommandBufferSubmitInfo`:
- Remove `TimelineSemaphoreSubmitInfo` pNext shim
- Each semaphore carries its own `.value()` and `.stage_mask()`

Files to check:
- `vulkan/rhi/vulkan_blitter.rs`
- `vulkan/rhi/vulkan_command_buffer.rs`
- Any remaining calls that #253 didn't migrate

### 3. Extension cleanup in vulkan_device.rs

Remove from `device_extensions`:
- `VK_KHR_dynamic_rendering` (core since 1.3)
- `VK_KHR_synchronization2` (core since 1.3)

Keep the feature enable structs — still required at device creation.

### 4. Evaluate additional 1.4 promotions

Check if any of these apply:
- `VK_KHR_copy_commands2` — `cmd_copy_buffer_2`, `cmd_copy_image_2`
- `VK_KHR_maintenance4` — buffer memory requirements without creating buffer
- `VK_KHR_format_feature_flags2` — extended format queries

## Verification

Grep confirms zero remaining old-pattern usage:
- `cmd_pipeline_barrier(` (without `_2`) → 0 hits in non-test code
- `queue_submit(` (without `_2`) → 0 hits in non-test code
- `TimelineSemaphoreSubmitInfo` → 0 hits
- `VK_KHR_dynamic_rendering` / `VK_KHR_synchronization2` in device_extensions → 0 hits

## Testing goals

- `cargo build` clean
- `cargo clippy` clean
- All existing tests pass
- E2E camera-display validation passes
- No performance regression (expect improvement from precise stage masks)
