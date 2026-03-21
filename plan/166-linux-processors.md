---
whoami: amos
name: "@github:tatolab/streamlib#166"
description: Linux — Processors — camera, display, audio, MP4 writer, screen capture
dependencies:
  - "down:@github:tatolab/streamlib#163"
  - "down:@github:tatolab/streamlib#164"
  - "down:@github:tatolab/streamlib#165"
  - "down:@github:tatolab/streamlib#167"
  - "down:@github:tatolab/streamlib#178"
  - "down:@github:tatolab/streamlib#180"
  - "up:@github:tatolab/streamlib#168"
adapters:
  github: builtin
---

@github:tatolab/streamlib#166

Phase 4 of the Linux support plan. Linux I/O processors.

### AI context (2026-03-21)
- Phase 1 (Vulkan RHI) is complete — `GpuContext`, pixel buffers, blitter all work on Linux
- **HARD BLOCKER: #178 (Cross-platform PixelFormat) must land first.** Linux PixelFormat is currently `{ Unknown }` — processors need Bgra32, Nv12, etc.
- `VulkanFormatConverter::convert()` returns NotSupported — camera/FFmpeg may need NV12→RGBA. Either implement or use CPU fallback.
- Display processor needs `VK_KHR_swapchain` which is not yet in VulkanDevice

### Priority
- **P2**: Audio capture/output (CPAL may already work on Linux with minimal changes)
- **P3**: Camera (V4L2), Display (Vulkan + winit)
- **P4**: MP4 writer (depends on #167 FFmpeg)
- **P5**: Screen capture (PipeWire portal or X11)

### Wiring
`lib.rs` re-exports: `LinuxCameraProcessor as CameraProcessor`, etc.

### Depends on
- #178 (Cross-platform PixelFormat) — **hard blocker**
- #163 (Vulkan RHI) — complete
- #164 (Broker) — multi-process pipelines
- #165 (Platform services) — audio clock for audio processors
- #167 (FFmpeg) — MP4 writer needs encoder/muxer
