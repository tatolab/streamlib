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

### Current Display Issue (needs fresh investigation)

**Symptom:** Display window shows vertical color banding/striping, pink wash on right side, green diagonal band. Not the camera feed.

**What works:**
- Decode: 200+ frames, all status OK, no device lost
- Encoder: 1000+ frames stable
- NV12→BGRA conversion runs without errors
- Display window opens and renders (not black — actual pixel data is shown)

**What's been verified:**
- Shader source `nv12_to_bgra.comp` looks correct (Y/UV plane reads, BT.601 color matrix, BGRA packing)
- Descriptor bindings use actual buffer sizes (`src_vk.size()`, `dst_vk.size()`), NOT the `source_bytes_per_pixel` field
- NV12 buffer: 1920x1088 coded height, bpp=12, size=3,133,440 bytes
- BGRA buffer: 1920x1088 coded height, bpp=32, size=8,355,840 bytes
- Videoframe reports display height 1080, but pixel buffer dimensions are 1088
- Dispatch: `(1920+15)/16=120, (1088+15)/16=68` → 1920x1088 exact fit, no overflow
- NV12 transfer copies full coded height (1088 rows Y + 544 rows UV)
- Push constants: width=1920, height=1088, flags=1 (is_bgra=true, full_range=false)

**What has NOT been verified:**
- Whether the NV12 data from the Vulkan Video decoder's DPB image is in the expected byte layout (Y plane contiguous, then interleaved UV)
- Whether the display processor's `cmd_copy_buffer_to_image` for B8G8R8A8_UNORM correctly interprets the BGRA buffer at 1088 height
- Whether there's a race condition between the format converter queue submission and the display processor reading the buffer
- Whether the camera path's working BGRA output is truly identical in buffer layout to what the format converter produces
- Whether the NV12 buffer pool reuse is causing stale data from a previous frame

**Hypothesis to test next:**
1. The NV12 data layout from Vulkan Video decode may use a different plane arrangement than what the shader expects (e.g., planar vs semi-planar, or different UV ordering U/V vs V/U)
2. The DPB image's NV12 format (G8_B8R8_2PLANE_420_UNORM) may have a different memory layout than tightly-packed NV12 — planes may have alignment/padding between them
3. The display processor may be reading from the NV12 buffer instead of the BGRA buffer (wrong surface_id mapping)

**Key files:**
- `libs/streamlib/src/vulkan/rhi/vulkan_video_decoder.rs` — decode + NV12 transfer + BGRA conversion
- `libs/streamlib/src/vulkan/rhi/vulkan_format_converter.rs` — compute shader dispatch
- `libs/streamlib/src/vulkan/rhi/shaders/nv12_to_bgra.comp` — the actual shader
- `libs/streamlib/src/linux/processors/display.rs` — display rendering pipeline
- `libs/streamlib/src/vulkan/rhi/vulkan_pixel_buffer.rs` — pixel buffer allocation

**Working reference:**
- The camera path (camera → BGRA pixel buffer → display) works perfectly
- The encoder path (BGRA → NV12 via format converter → Vulkan Video encode) works perfectly
- The decoder decode path (H.264 → Vulkan Video decode → NV12 DPB image) works perfectly
- Only the decoder output path (NV12 DPB → NV12 buffer → BGRA buffer → display) shows corruption
