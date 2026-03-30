---
whoami: amos
name: "@github:tatolab/streamlib#200"
description: P0 — Linux rendering performance parity with macOS — 6-phase GPU optimization plan
dependencies:
  - "down:@github:tatolab/streamlib#166"
  - "down:@github:tatolab/streamlib#163"
adapters:
  github: builtin
---

@github:tatolab/streamlib#200

P0 blocker: Linux rendering has critical performance gaps vs macOS. 6 phases.

Context: macOS gets zero-copy GPU pipelines for free via CoreVideo/IOSurface/Metal/AVFoundation. Linux must build equivalent with Vulkan compute shaders, render pipelines, and explicit memory management. Architecture follows Unreal Engine approach — platform-specific deep optimization behind the RHI abstraction. No macOS code changes.

---

### Phase 1: Fix display polling + multi-flight rendering ⏳ PAUSE — verify display behavior
**Impact: HIGH | Effort: LOW | Files: display.rs only**

- [ ] Remove `ControlFlow::Poll` spin-loop (display.rs:116)
- [ ] Remove `thread::sleep(500μs)` no-frame fallback (display.rs:367-369)
- [ ] Per-swapchain-image sync sets (fences + semaphores) — replaces single in_flight_fence
- [ ] Remove second `wait_for_fences` at line 610 (double fence wait per frame)
- [ ] Pre-allocate command buffers per swapchain image, reset instead of alloc/free

### Phase 2: GPU compute shader for NV12/YUYV→BGRA ⏳ PAUSE — verify shader output
**Impact: CRITICAL | Effort: MEDIUM | New files: shaders/**

- [ ] Create `linux/processors/shaders/nv12_to_bgra.comp` — GLSL compute shader
- [ ] Create `linux/processors/shaders/yuyv_to_bgra.comp` — GLSL compute shader
- [ ] Compile to SPIR-V (build-time via shaderc or checked-in .spv)
- [ ] Camera.rs: upload raw V4L2 data to HOST_VISIBLE SSBO
- [ ] Camera.rs: VkComputePipeline + descriptor set layout/pool + push constants
- [ ] Compute shader writes to device-local VkImage (STORAGE | SAMPLED)
- [ ] Remove CPU scalar float conversion functions (camera.rs:313-413)

### Phase 3: Vulkan fullscreen render pipeline ⏳ PAUSE — verify scaling/aspect-ratio
**Impact: HIGH | Effort: MEDIUM | New files: shaders/, vulkan_device.rs change**

- [ ] Create `linux/processors/shaders/fullscreen.vert` — fullscreen triangle (no vertex buffer)
- [ ] Create `linux/processors/shaders/fullscreen.frag` — aspect-ratio-aware sampling + black bars
- [ ] Enable `VK_KHR_dynamic_rendering` in vulkan_device.rs (out-of-scope, approved)
- [ ] Display.rs: VkGraphicsPipeline + VkSampler (LINEAR, CLAMP_TO_EDGE)
- [ ] Display.rs: descriptor set for combined image sampler
- [ ] Display.rs: push constants for aspect-ratio scale/offset
- [ ] Display.rs: replace `cmd_copy_buffer_to_image` with `vkCmdDraw(3, 1, 0, 0)`

### Phase 4: Camera resolution negotiation
**Impact: MEDIUM | Effort: LOW | Files: camera.rs only**

- [ ] Enumerate V4L2 frame sizes via `dev.enum_framesizes(fourcc)`
- [ ] Pick highest resolution (or closest to display resolution)
- [ ] Set `try_fmt.width/height` before `set_format()`
- [ ] Fallback to device default if enumeration fails

### Phase 5: Vulkan RHI sync fixes + VulkanFormatConverter GPU migration
**Impact: MEDIUM | Effort: MEDIUM | Files: vulkan/ RHI layer**

- [ ] VulkanBlitter::blit_copy() — replace `queue_wait_idle` with fence (vulkan_blitter.rs:88-93)
- [ ] VulkanCommandBuffer::commit_and_wait() — replace `queue_wait_idle` with fence (vulkan_command_buffer.rs:161-187)
- [ ] VulkanCommandBuffer::commit() — fence + deferred cleanup for race condition
- [ ] Async transfer queue — discover dedicated TRANSFER queue family in VulkanDevice (vulkan_device.rs:177-182)
- [ ] VulkanFormatConverter — migrate CPU scalar conversion to same GPU compute shader approach as Phase 2

### Phase 6: Timeline semaphores + V4L2 DMABUF with runtime probe
**Impact: LOW-MED | Effort: HIGH | Requires runtime driver probing**

- [ ] Timeline semaphores (Vulkan 1.2 core) — replace binary semaphores for internal sync
- [ ] V4L2 DMABUF zero-copy — probe at runtime, use on AMD/Intel, fallback to memcpy on NVIDIA
- [ ] `VK_EXT_image_drm_format_modifier` enablement for DMA-BUF VkImage import
- [ ] NVIDIA limitation: cross-device DMA-BUF import fails on proprietary drivers (documented)

---

### Depends on
- #166 (Linux processors) — merged ✅
- #163 (Vulkan RHI) — complete ✅

### NVIDIA-specific note
Cross-device DMA-BUF import (V4L2 USB camera → NVIDIA Vulkan) does NOT work reliably on NVIDIA proprietary drivers. Primary data path: V4L2 MMAP → memcpy HOST_VISIBLE SSBO → GPU compute → device-local VkImage → fullscreen shader → swapchain. DMABUF zero-copy deferred to Phase 6 with runtime probe.
