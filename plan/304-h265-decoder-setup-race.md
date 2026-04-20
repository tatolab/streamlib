---
whoami: amos
name: '@github:tatolab/streamlib#304'
adapters:
  github: builtin
description: Flaky H.265 decoder DEVICE_LOST during setup — One-in-N runs of vulkan-video-roundtrip h265 /dev/video2 hit a DEVICE_LOST on the H.265 decoder between first-frame-encoded and display swapchain creation. Retry passes clean. Suspected concurrent-Vulkan-ops race during decoder/display setup, same window h264_encoder.rs already guards via device_wait_idle.
github_issue: 304
---

@github:tatolab/streamlib#304

## Branch

Create `fix/h265-decoder-setup-race` from `main`.

## Steps

1. Reproduce by running `vulkan-video-roundtrip h265 /dev/video2 15` in a loop (≥ 20 iterations) and confirm the flake rate.
2. Add `tracing::debug!` around every VIDIOC/device-level call in `H265DecoderProcessor::setup()` and `DisplayProcessor::setup()` so a failing run shows exactly which thread ran which call at each timestamp.
3. Mirror the encoder-side pattern: append `vulkan_device.device().device_wait_idle()` at the end of `H264DecoderProcessor::setup()` and `H265DecoderProcessor::setup()` (after `pre_initialize_session` + pixel-buffer probe).
4. Consider hoisting `device_wait_idle` into a `StreamRuntime` setup barrier so every processor's `setup()` runs against a quiesced device by construction — removes per-codec-processor boilerplate and is forward-proof for new codecs.

## Verification

- `vulkan-video-roundtrip h265 /dev/video2 15` passes 20 consecutive runs (zero `DEVICE_LOST`, each produces ≥ 1 PNG).
- Same for `h265 /dev/video0` (Cam Link).
- H.264 vivid/Cam Link still pass per [`docs/testing.md`](../docs/testing.md) with PNG Read-tool verification.
- File the standardized [test-report](../docs/testing.md#standardized-test-output-template) summarizing all four runs.

## References

- PR #301 retest comment (first observation): https://github.com/tatolab/streamlib/pull/301#issuecomment-4274680105
- Existing encoder-side mitigation: `libs/streamlib/src/linux/processors/h264_encoder.rs:90-95`
- [`docs/learnings/nvidia-dual-vulkan-device-crash.md`](../docs/learnings/nvidia-dual-vulkan-device-crash.md)
