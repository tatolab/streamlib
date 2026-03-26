---
whoami: amos
name: "@github:tatolab/streamlib#207"
description: Vulkan Video H.264/H.265 encoder — zero-copy GPU encoding
dependencies:
  - "down:@github:tatolab/streamlib#197"
adapters:
  github: builtin
---

@github:tatolab/streamlib#207

Replace CPU-bound FFmpeg encoding with Vulkan Video hardware encoding. Eliminates all CPU pixel copies — frames stay on GPU from camera capture through H.264/H.265 encode.

### Motivation
Current Linux encode path: Vulkan GPU texture → CPU memcpy (8MB/frame) → CPU swscale BGRA→YUV420P → FFmpeg encoder (NVENC uploads back to GPU). This causes ~280MB/s allocator churn and unnecessary CPU↔GPU round trips. Camera-display renders perfectly but WebRTC streaming is choppy due to encode overhead.

Vulkan Video keeps the entire pipeline on-GPU: Vulkan texture → Vulkan compute (BGRA→NV12) → Vulkan Video encode → encoded bitstream in host-visible buffer. Zero CPU pixel touching.

### Target environment
- Containerized Linux runtime with nvidia-container-toolkit
- NVIDIA cloud GPUs (T4, L4, A10G, A100, H100) — all support Vulkan Video encode
- Cross-vendor future: AMD and Intel also shipping Vulkan Video encode support

### Verified hardware support (RTX 3090)
- `VK_KHR_video_encode_h264` rev 14
- `VK_KHR_video_encode_h265` rev 14
- `VK_KHR_video_encode_queue` rev 12
- Dedicated VIDEO_ENCODE queue family

---

## Phase 1: Vulkan Video session infrastructure

### 1.1 Enable Vulkan Video device extensions
In `vulkan_device.rs`, enable during device creation:
- `VK_KHR_video_queue`
- `VK_KHR_video_encode_queue`
- `VK_KHR_video_encode_h264`
- `VK_KHR_video_encode_h265` (for Phase 4)

Request the video encode queue family (already detected: `QUEUE_VIDEO_ENCODE_BIT_KHR`).

### 1.2 Video session setup
Create a Vulkan Video session for H.264 Baseline encoding:
- `VkVideoSessionCreateInfoKHR` with H.264 encode profile
- `VkVideoSessionParametersCreateInfoKHR` with SPS/PPS
- Allocate and bind video session memory (device queries memory requirements)
- Create reference picture resources (DPB — decoded picture buffer)

### 1.3 Rate control
Configure rate control via `VkVideoEncodeRateControlInfoKHR`:
- CBR or VBR mode
- Target bitrate from VideoEncoderConfig
- GOP size and keyframe interval

---

## Phase 2: GPU colorspace conversion (BGRA → NV12)

### 2.1 Vulkan compute shader
Write a GLSL compute shader that converts BGRA → NV12 (NVENC's native input format):
- Input: `VkImage` in B8G8R8A8_UNORM (existing camera texture format)
- Output: `VkImage` in G8_B8R8_2PLANE_420_UNORM (NV12)
- Standard BT.601/BT.709 color matrix
- Dispatch: one workgroup per 2×2 pixel block (NV12 subsamples chroma 2:1 in both dimensions)

### 2.2 Pipeline setup
- Create compute pipeline with the BGRA→NV12 shader
- Descriptor set layout: 2 storage images (input BGRA, output NV12)
- Allocate NV12 output image with `VK_IMAGE_USAGE_VIDEO_ENCODE_SRC_BIT_KHR`

---

## Phase 3: Zero-copy encode pipeline

### 3.1 Encode command buffer
Record per-frame encoding:
1. Pipeline barrier: camera texture → compute shader read
2. Dispatch BGRA→NV12 compute shader
3. Pipeline barrier: NV12 image → video encode read
4. `vkCmdBeginVideoCodingKHR` — begin video coding scope
5. `vkCmdEncodeVideoKHR` — encode the NV12 frame
6. `vkCmdEndVideoCodingKHR` — end video coding scope
7. Encoded bitstream lands in a host-visible `VkBuffer`

### 3.2 Bitstream readback
- Query encode result via `VkVideoEncodeSessionFeedbackInfoKHR`
- Map the bitstream buffer, read the H.264 NAL units
- This is the ONLY CPU memory access — reading the small encoded bitstream (~10-50KB per frame vs 8MB raw)

### 3.3 Integration with VideoEncoder trait
Create `VulkanVideoEncoder` implementing the same interface as `FFmpegEncoder`:
- `new()`: Create video session, compute pipeline, allocate resources
- `encode()`: Submit compute + encode commands, read bitstream
- `set_bitrate()`: Reconfigure rate control
- `force_keyframe()`: Set `VK_VIDEO_CODING_CONTROL_ENCODE_INTRA_REFRESH_BIT_KHR`

### 3.4 Update VideoEncoder platform wrapper
In `video_encoder.rs`, add a new cfg path:
```rust
#[cfg(all(target_os = "linux", feature = "vulkan-video"))]
pub(crate) inner: crate::vulkan::video::VulkanVideoEncoder,
```
Feature gate: `vulkan-video` — opt-in since not all Linux deployments have Vulkan Video capable GPUs. Fall back to FFmpeg path when feature is off or GPU doesn't support it.

---

## Phase 4: H.265 and multi-codec

### 4.1 H.265 encode support
- Same pipeline, swap `VK_KHR_video_encode_h265` profile
- Different parameter set (VPS/SPS/PPS vs SPS/PPS)
- Better compression at same bitrate — important for cloud bandwidth costs

### 4.2 Runtime codec selection
- Query `vkGetPhysicalDeviceVideoCapabilitiesKHR` at startup
- Report available codecs
- VideoEncoderConfig already has a `codec` field — route to H.264 or H.265 video session

---

## Phase 5: Fallback and container integration

### 5.1 Runtime capability detection
At encoder creation, check:
1. Does the Vulkan device support video encode extensions?
2. Does it support the requested codec profile?
3. If not → fall back to FFmpeg encoder (existing code)

### 5.2 Container requirements
- Base image: nvidia/vulkan (provides Vulkan ICD + nvidia-container-toolkit integration)
- No CUDA dependency needed
- Driver requirement: NVIDIA 525+ for Vulkan Video encode

### 5.3 Testing
- Unit test: create video session on available hardware
- Integration test: encode a single synthetic frame, verify H.264 output
- Benchmark: compare encode latency vs FFmpeg path

---

## Dependencies
- `ash` crate (already in workspace) — provides Vulkan Video extension bindings
- GLSL compute shader compiled with `glslangValidator` or `shaderc`
- No new crate dependencies expected

## Risks
- Vulkan Video API is verbose (~200 lines for session setup)
- Driver bugs in video encode path (newer extension, less battle-tested than NVENC)
- Different GPUs may have different format/profile support — need robust capability queries
- DPB (decoded picture buffer) management adds complexity for B-frame support

## AI context (2026-03-25)
- RTX 3090 confirmed: H.264 + H.265 encode, dedicated encode queue
- Current FFmpeg path works but causes choppy WebRTC streaming due to CPU copies
- Camera textures are already Vulkan B8G8R8A8_UNORM images
- ash crate already in workspace, Vulkan RHI layer exists at vulkan/rhi/
