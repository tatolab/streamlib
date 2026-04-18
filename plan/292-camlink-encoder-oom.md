---
whoami: amos
name: Cam Link encoder ERROR_OUT_OF_DEVICE_MEMORY in debug
status: pending
description: Debug why the H.264/H.265 encoders fail every process() with OOM when capturing from Cam Link 4K (MMAP+memcpy path) while vivid works.
github_issue: 292
adapters:
  github: builtin
---

@github:tatolab/streamlib#292

## Branch

Create `fix/camlink-encoder-oom` from `main`.

## Steps

1. Reproduce: `cargo run -p vulkan-video-roundtrip -- h264 /dev/video0 15`. Expect ~890 `ERROR_OUT_OF_DEVICE_MEMORY` warnings and `frames_encoded=0`.
2. Enable VMA allocator stats (`VMA_DEBUG_*`) to identify which pool fails and which VMA call.
3. Audit whether the MMAP fallback's staging buffers are being allocated in a DMA-BUF exportable pool unnecessarily.
4. Consider eager pre-allocation of the encoder's DPB / bitstream buffers before swapchain creation (see @docs/learnings/nvidia-dma-buf-after-swapchain.md).
5. Investigate USERPTR or direct-DMA-BUF UVC modes to avoid the memcpy path entirely.

## Verification

- Cam Link + H.264 debug round-trips ≥ 15 s without OOM.
- Cam Link + H.265 debug round-trips ≥ 15 s without OOM.
