# Camera-display E2E validation without windows or physical hardware

## When you need this

You changed anything in the GPU pipeline (`vulkan_device.rs`,
`vulkan_pixel_buffer.rs`, `vulkan_texture.rs`, `linux/processors/camera.rs`,
`linux/processors/display.rs`) and need to confirm:

- Pipeline runs end-to-end without OOM or driver errors
- Frames actually render (not just black/empty)
- Process exits cleanly (no stranded windowed processes)

Don't try to reproduce GPU bugs in pure unit tests with mocked swapchains
— most NVIDIA driver issues require live compositor + concurrent GPU
work and won't trigger in isolation. See
@docs/learnings/nvidia-dma-buf-after-swapchain.md.

## One-time host setup

```bash
# v4l2loopback kernel module — virtual camera device
sudo apt-get install v4l2loopback-dkms
sudo modprobe v4l2loopback video_nr=10 card_label=Virtual_Camera exclusive_caps=0
# Note: exclusive_caps=0 (NOT 1) — caps=1 breaks ffmpeg→v4l2loopback writes

# Tools
sudo apt-get install ffmpeg imagemagick xdotool
```

## Run

```bash
libs/streamlib/tests/fixtures/e2e_camera_display.sh /tmp/streamlib-e2e
```

The script:
1. Starts ffmpeg streaming a `testsrc` pattern to `/dev/video10`
2. Runs `cargo run -p camera-display` with debug env vars (see below)
3. Validates: DMA-BUF pools created, swapchain created, first frame
   captured, zero OOM errors, ≥1 PNG sample produced

Exit codes: 0 = pass, 1 = fail, 77 = skipped (prerequisites missing).

## AI-tappable validation

PNG samples land in `$OUTPUT_DIR/png_samples/` as full-resolution
1920x1080 BGRA→RGBA PNGs. Read them directly with the Read tool to
visually verify pipeline correctness. The PNG writer is dependency-free
(handwritten in `display.rs`) so adding more dependencies is unnecessary.

The samples come from the source HOST_VISIBLE pixel buffer (not a GPU
readback of the rendered swapchain image), so they validate the camera→
display data flow but NOT the actual rendering of the swapchain.

## Debug env vars (read by display.rs at start())

| Var | Effect |
|---|---|
| `STREAMLIB_CAMERA_DEVICE=/dev/videoN` | Override default `/dev/video0` |
| `STREAMLIB_DISPLAY_FRAME_LIMIT=N` | Auto-exit after N rendered frames (avoids stranded windows) |
| `STREAMLIB_DISPLAY_PNG_SAMPLE_DIR=path` | Save sample frames as PNG |
| `STREAMLIB_DISPLAY_PNG_SAMPLE_EVERY=N` | Sampling interval (default 30) |

## Troubleshooting

**"Failed to read current format: Invalid argument" from camera startup**
ffmpeg isn't actually streaming to `/dev/video10`. Restart it via the
fixture script: `libs/streamlib/tests/fixtures/virtual_camera.sh start`.
Verify with `v4l2-ctl -d /dev/video10 --get-fmt-video` — should show
`1920x1080 YUYV`. If it shows "Invalid argument", the v4l2loopback module
needs to be loaded with `exclusive_caps=0` (not 1).

**"EventLoop can't be recreated" in unit tests**
winit's `EventLoop` is per-PROCESS on Linux X11 — only one per process.
For multi-scenario unit tests, build the EventLoop once and call
`event_loop.run_app_on_demand()` per scenario.

**Process strands after timeout / Ctrl+C**
Window-based runs sometimes don't respect SIGTERM cleanly (winit + X11
interaction issue). Always use `timeout --kill-after=3 N ...` to force
SIGKILL after a grace period, then `pkill -9 -f camera-display` defensively.

## Reference
- Fixture scripts: `libs/streamlib/tests/fixtures/`
- Display debug feature implementation: `libs/streamlib/src/linux/processors/display.rs` (search `png_sample_dir`)
