# Camera-display E2E validation without physical hardware

## When you need this

You changed anything in the GPU pipeline (`vulkan_device.rs`, `vulkan_buffer.rs`,
`vulkan_texture.rs`, `runtime/streamlib-engine/src/linux/v4l2_video_device_backend.rs`,
`runtime/streamlib-engine/src/core/context/captured_video_frame_to_pooled_rgba_conversion_stage.rs`,
`runtime/streamlib-media-builtins/src/camera_source.rs`,
`runtime/streamlib-media-builtins/src/display_window.rs`) and need to confirm:

- Pipeline runs end-to-end without OOM or driver errors
- Frames actually render (not just black/empty)
- Process exits cleanly (no stranded windowed processes)

Don't try to reproduce GPU bugs in pure unit tests with mocked swapchains
— most NVIDIA driver issues require live compositor + concurrent GPU
work and won't trigger in isolation. See
@docs/learnings/nvidia-dma-buf-after-swapchain.md.

## One-time host setup

The fixture drives the in-kernel **vivid** V4L2 test driver — no DKMS and no
out-of-tree module:

```bash
sudo modprobe vivid
sudo apt-get install xdotool x11-apps python3-pil   # xwd ships in x11-apps
```

v4l2loopback + an ffmpeg `testsrc` remains the setup for the *motion* scenario
(a visible per-frame counter, for drop/repeat bugs), and carries one non-obvious
constraint worth keeping: load it with `exclusive_caps=0`, **not** `1` — `caps=1`
breaks ffmpeg→v4l2loopback writes.

```bash
sudo modprobe v4l2loopback video_nr=10 card_label=Virtual_Camera exclusive_caps=0
```

## Run

```bash
runtime/streamlib-engine/tests/fixtures/e2e_camera_display.sh /tmp/streamlib-e2e
```

The script:
1. Loads vivid and finds its capture node
2. Boots `camera_display_stream.py`, the fixture stream beside the script, with
   `tatolab run` on the runtime unit (`cargo xtask build-runtime`), compiled in
   the fixture venv — the stream is Python, so there is no build step between an
   edit and the run
3. Waits for the `tatolabd` it started to register, then asserts against
   `streamlib graph` (the observation verbs, run from the runtime unit's lend):
   both native built-ins present, linked camera → window
4. Captures the window to PNG, then sends SIGTERM to `tatolab run`, which
   forwards it to `tatolabd`, and requires a clean exit
5. Gates the log on `OUT_OF_DEVICE_MEMORY` / `DEVICE_LOST` / `process() failed`

Exit codes: 0 = pass, 1 = fail, 77 = skipped (prerequisites missing).

## Assert on contracts, not on tracing prose

Gate on things the plan makes durable: the `graph` tool's JSON shape, the JSONL
log schema, process exit status, and the pixels in a captured PNG. Vulkan error
strings (`OUT_OF_DEVICE_MEMORY`, `DEVICE_LOST`) are also stable — they come from
the driver, not from us. Our own `tracing` messages are not a test API: they get
renamed or deleted in refactors, and a gate that greps them then reports FAIL on
a healthy run.

## AI-tappable validation

The window capture lands in `$OUTPUT_DIR/png_samples/window.png`, grabbed with
`xdotool search --name` → `xwd` → PIL (`tests/fixtures/capture_window.py`).
Read it with the Read tool and describe what it shows.

Because it is a capture of the composited window, it validates the *whole* path
including the swapchain present — a dump of the source HOST_VISIBLE pixel buffer,
taken before rendering, would not.

The fixture selects the camera through `STREAMLIB_CAMERA_DEVICE`, which
`camera_display_stream.py` reads — the engine does not.

## Troubleshooting

**"Failed to read current format: Invalid argument" from camera startup**
ffmpeg isn't actually streaming to `/dev/video10`. Restart it via the
fixture script: `runtime/streamlib-engine/tests/fixtures/virtual_camera.sh start`.
Verify with `v4l2-ctl -d /dev/video10 --get-fmt-video` — should show
`1920x1080 YUYV`. If it shows "Invalid argument", the v4l2loopback module
needs to be loaded with `exclusive_caps=0` (not 1).

**"EventLoop can't be recreated" in unit tests**
winit's `EventLoop` is per-PROCESS on Linux X11 — only one per process.
For multi-scenario unit tests, build the EventLoop once and call
`event_loop.run_app_on_demand()` per scenario.

**Process strands after timeout / Ctrl+C**
Window-based runs sometimes don't respect SIGTERM cleanly (winit + X11
interaction issue). The fixture waits 15s after SIGTERM and then escalates to
SIGKILL *inline*, before reaping — deliberately not from the `trap`, because a
`wait` on a process that ignores SIGTERM blocks forever and the EXIT trap cannot
fire while the script is blocked in it. A hung fixture reports nothing; a killed
one reports the failure. A run that needs the SIGKILL is a finding, not a flake —
the interpreter-lifecycle contract says engine teardown precedes interpreter
finalization.

## Reference
- Fixture scripts: `runtime/streamlib-engine/tests/fixtures/`
- The stream under test: `runtime/streamlib-engine/tests/fixtures/camera_display_stream.py`
