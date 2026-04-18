---
whoami: amos
name: Decoder pool probe uses hard-coded resolution
status: pending
description: Replace the hard-coded 1920x1088 probe in H264/H265 decoder setup() with a probe derived from decoder config / session capabilities so non-1080p streams don't regress #292.
github_issue: 302
adapters:
  github: builtin
---

@github:tatolab/streamlib#302

## Branch

Create `fix/decoder-probe-dynamic-resolution` from `main`.

## Steps

1. In `libs/vulkan-video/src/decode/` expose the codec-aligned output extent on `SimpleDecoder` (use the existing `max_width`/`max_height` from `SimpleDecoderConfig` plus codec macroblock alignment). Preferred API: `SimpleDecoder::prepare_gpu_decode_resources()` that mirrors `SimpleEncoder::prepare_gpu_encode_resources()` from #301 and internally knows the aligned output extent.
2. Replace the hard-coded probe in `libs/streamlib/src/linux/processors/h264_decoder.rs` and `h265_decoder.rs`:
   ```rust
   let (_probe_id, _probe_buffer) =
       ctx.gpu.acquire_pixel_buffer(1920, 1088, crate::core::rhi::PixelFormat::Rgba32)?;
   ```
   with a call to the new method (or with `acquire_pixel_buffer(aligned_w, aligned_h, ...)` read from the decoder).
3. Keep the call site **before** the display swapchain is created (i.e., still in `setup()` after `pre_initialize_session`).

## Verification

- Cam Link `/dev/video0` H.264 + H.265 at 1920x1080 (release and debug) remain OOM-free — regression check for #292. Follow the encoder/decoder protocol in [`docs/testing.md`](../docs/testing.md), including PNG Read-tool verification.
- Add a non-1080p roundtrip scenario (e.g., 1280x720 via vivid config, or a 4K Cam Link mode) that runs camera→encoder→decoder→display with zero `OUT_OF_DEVICE_MEMORY` and valid PNG output.
- Optional: unit test on `SimpleDecoder::prepare_gpu_decode_resources()` / aligned extent math.

## References

- PR #301 (introduced the hard-coded probe as part of the #292 fix)
- Issue #292 (Cam Link encoder OOM — root cause + encoder-side pattern to mirror)
- [`docs/learnings/nvidia-dma-buf-after-swapchain.md`](../docs/learnings/nvidia-dma-buf-after-swapchain.md)
- [`docs/testing.md`](../docs/testing.md)
