---
whoami: amos
name: '@github:tatolab/streamlib#167'
description: Linux — FFmpeg codec integration — H.264 encode/decode/mux
adapters:
  github: builtin
blocked_by:
- '@github:tatolab/streamlib#166'
blocks:
- '@github:tatolab/streamlib#163'
- '@github:tatolab/streamlib#178'
---

@github:tatolab/streamlib#167

Phase 5 of the Linux support plan. FFmpeg-based H.264 encoding/decoding/muxing.

### AI context (2026-03-21)
- GPU texture readback works via `VulkanPixelBuffer::mapped_ptr()` — direct CPU access to pixel data
- **HARD BLOCKER: #178 (Cross-platform PixelFormat) must land first.** Encoder/decoder need NV12, RGBA format awareness.
- 1.6 (VulkanFormatConverter) moved here from #163 — `convert()` returns NotSupported, needs implementation for NV12↔RGBA codec I/O
- Stubs exist in `linux/ffmpeg/` (encoder, decoder, muxer) — all return "not yet implemented"

### Hardware acceleration
- NVENC (`h264_nvenc`) — NVIDIA
- VAAPI (`h264_vaapi`) — Intel/AMD
- V4L2 M2M (`h264_v4l2m2m`) — embedded/SoC

### Depends on
- #178 (Cross-platform PixelFormat) — **hard blocker**
- #163 (Vulkan RHI) — complete
