# Vulkan Video Decoder — Debug Progress

## Baseline (before #233)
- Encoder: 2000+ frames stable at ~60fps
- MoQ: publish/subscribe flowing via Cloudflare relay
- Display: rendering camera feed (FFmpeg decoder)

## Changes Made (this branch)
1. Removed FFmpeg entirely (decoder, encoder fallback, muxer, features, deps)
2. Added VulkanVideoDecoder + VulkanVideoDecodeSession in vulkan/rhi/
3. Wired into VideoDecoder wrapper (Vulkan-only on Linux)
4. Fixed SPS RBSP extraction (strip NAL header byte)
5. Fixed SPS-derived coded dimensions (not config defaults)
6. Added DPB empty guard (no P-frame decode without refs)
7. Added resource cleanup on partial init_resources failure
8. Added pending NAL cache (IDR survives init retry)
9. Added decode status query pool
10. Fixed DPB coincide mode (NVIDIA requires same image for output+DPB)
11. Fixed Annex B start codes in bitstream buffer
12. Fixed bitstream buffer range alignment
13. Fixed image layout transitions for coincide mode
14. Fixed query pool reset placement (outside video coding scope)
15. Fixed NV12 pixel buffer size (integer division: 12/8=1, should be 12*w*h/8)
16. Fixed fence double-wait on P-frames
17. Fixed post-transfer DPB layout transition
18. Fixed frame_num/POC derivation
19. Added secondary graphics queue for decoder transfers
20. Vendored moq-transport with FilterType::NextGroupStart patch
21. Added debug diagnostic frames (disabled by default)
22. Demoted encoder per-frame logs to debug level

## Current State
- Decode: IDR + P-frames decode successfully (verified 3 frames, status OK)
- Encoder: REGRESSION — crashes at ~72 frames (was 2000+ on main)
- Display: never verified (encoder dies before sustained decode)
- MoQ IDR: delivered via NextGroupStart patch

## Step 1: Identify encoder regression
- Status: FOUND
- Root cause: NV12 buffer sized for 1920x1080 (3,110,400 bytes) but compute shader
  dispatches rounded up to 1920x1088 (3,133,440 bytes) — 23,040 byte overflow
- The old bpp=2 buffer was 4,147,200 bytes with enough headroom
- The new exact-fit buffer has zero headroom for dispatch rounding
- Fix: use dispatch-aligned height (1088) for buffer allocation, not raw 1080

## Step 2: Fix encoder stability
- Status: DONE
- Fix: encoder NV12 staging buffers use bpp=16 (w*h*2) for dispatch headroom
- Verified: no device lost in user test run

## Step 3: Verify decoder receives IDR and decodes continuously
- Status: DONE
- Verified: 55+ frames decoded continuously, all decode status OK
- DPB management working (active_refs=2, proper eviction)

## Step 4: Verify frames reach display
- Status: PARTIAL — display shows garbled stripes/colors, not camera feed
- NV12→BGRA format conversion added via VulkanFormatConverter compute shader
- Both encoder and decoder stable (1000+ frames each)
- Display window opens, renders frames, but content is garbled

### Display Rendering Pipeline — FIXED (2026-04-05)

Two display-related issues fixed:

1. **vkCmdCopyImageToBuffer garbled output** (FIXED): NVIDIA driver does not correctly de-tile
   multi-planar G8_B8R8_2PLANE_420_UNORM images from Vulkan Video decode when copying to buffer.
   Only ~15% of image rows contain unique data; rest repeats, creating vertical stripes.
   **Fix:** Replaced image-to-buffer copy with per-plane sampled image compute shader (`dpb_to_bgra.comp`)
   that reads DPB PLANE_0 (R8_UNORM, Y) and PLANE_1 (R8G8_UNORM, UV) directly via texture sampling.
   Eliminates NV12 intermediate buffer entirely.

2. **MoQ subscriber never receives IDR** (FIXED): The MoQ publish session created new subgroups
   for each keyframe but never created new groups. The NextGroupStart filter waits for new groups,
   not subgroups, so the subscriber always joined mid-GOP and got only P-frames.
   **Fix:** `publish_frame()` now drops the SubgroupsWriter on keyframe, creating a new MoQ group
   per GOP so NextGroupStart delivers the IDR at each group boundary.

3. **Inter-queue semaphore** (FIXED): Added vk::Semaphore between video decode queue submission
   and graphics queue submission for GPU-to-GPU synchronization.

### Decode Output: RESOLVED for Baseline (2026-04-06)

**H.264 Baseline (CAVLC) decode: WORKING.** Camera→Encode→Decode→Display pipeline
verified pixel-perfect against FFmpeg software decode. Status=1 (VK_QUERY_RESULT_STATUS_COMPLETE_KHR).

**H.264 Main/High (CABAC) decode: BROKEN.** Garbled vertical stripes, decode status=1000331003
(not the expected status=1). Issue is CABAC-specific — Baseline (CAVLC) works with identical
code paths. Both libx264 and Vulkan Video encoder bitstreams produce the same garbling when
decoded with Main/High profile. Root cause is in the session parameter setup for CABAC — needs
comparison with FFmpeg's vulkan_h264.c. Deferred; AV1 is the next codec target.

**Decision:** H.264 encoder/decoder enforced Baseline-only. Main/High support dropped.

**Fixes applied (2026-04-06):**
1. `slotIndex=-1` in `VkVideoBeginCodingInfoKHR` setup slot (Vulkan spec compliance)
2. CONCURRENT sharing mode + `QUEUE_FAMILY_IGNORED` barriers (matches all reference impls)
3. Bitstream buffer barrier (host write → video decode read)
4. PPS `weighted_bipred_idc` and `transform_8x8_mode_flag` parsed correctly
5. `VK_KHR_video_maintenance1` extension enabled
6. Encoder enforced Baseline profile
