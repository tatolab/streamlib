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
- Status: FIX APPLIED — needs user verification
- Root cause: display processor expects BGRA (B8G8R8A8_UNORM), decoder was outputting NV12
- Fix: added VulkanFormatConverter (NV12→BGRA) in decoder's init_resources
- Decoder now: decode → NV12 DPB → transfer to NV12 buffer → convert to BGRA buffer → output
- Compiles clean, needs user to run and verify video in display window
