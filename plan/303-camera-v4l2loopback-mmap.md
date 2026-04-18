---
whoami: amos
name: Camera MMAP path sees 0 frames on v4l2loopback
status: completed
description: LinuxCameraProcessor opens /dev/video10 (v4l2loopback) cleanly, logs V4L2 capture started, then its poll loop receives zero frames — while v4l2-ctl and ffmpeg on the same device get frames immediately. Blocks using v4l2loopback + ffmpeg testsrc2 as a deterministic motion fixture.
github_issue: 303
adapters:
  github: builtin
---

@github:tatolab/streamlib#303

## Branch

Create `fix/camera-v4l2loopback-mmap` from `main`.

## Steps

1. Reproduce: load v4l2loopback with `exclusive_caps=0`, feed it with `ffmpeg -re -f lavfi -i 'testsrc2=size=1920x1080:rate=30,format=nv12' -f v4l2 /dev/video10`, then run `STREAMLIB_CAMERA_DEVICE=/dev/video10 cargo run -p camera-display`. Expect "V4L2 capture started" followed by "Stopped (0 frames)".
2. Narrow: `strace -e ioctl` the camera processor thread, or add targeted `tracing::debug` around `v4l::io::mmap::Stream::with_buffers`, the poll loop, and the post-poll `stream.next()` call. Compare which VIDIOC calls differ vs. `v4l2-ctl --stream-mmap=3`.
3. Likely suspects (in order): the hard-coded `V4L2_BUFFER_COUNT = 4` in `libs/streamlib/src/linux/processors/camera.rs` exceeds v4l2loopback's reported capacity (make it adaptive to the `VIDIOC_REQBUFS` response); the poll FD chosen by the `v4l` crate doesn't match the actual streaming FD; `VIDIOC_STREAMON` ordering relative to QBUF.
4. Once frames flow, extend [`docs/testing.md`](../docs/testing.md) with a **motion scenario** that uses v4l2loopback + `testsrc2` (scrolling timecode + frame counter) so frame-drop assertions become tractable.

## Verification

- `cargo run -p camera-display` against `/dev/video10` (fed by `ffmpeg testsrc2`) captures ≥ 1 frame within 5 s and PNG samples show the testsrc2 pattern (color bars + diagonal moving line + timecode overlay). Describe the content per the [test-report template](../docs/testing.md#standardized-test-output-template).
- `vulkan-video-roundtrip h264 /dev/video10` and `h265 /dev/video10` both roundtrip for 15 s with zero `OUT_OF_DEVICE_MEMORY`, `DEVICE_LOST`, or `process() failed`; encoder/decoder first-frame markers fire; PNG samples show the expected pattern.
- Send at least one PNG per codec to the user's Telegram chat for visual confirmation.

## References

- Regression observed during PR #301 motion-testing scope conversation.
- [`docs/learnings/camera-display-e2e-validation.md`](../docs/learnings/camera-display-e2e-validation.md) — documents the historical v4l2loopback trouble and recommends vivid as the current synthetic source.
- [`docs/testing.md`](../docs/testing.md) — where the new motion scenario will land once this fix makes v4l2loopback consumable.
