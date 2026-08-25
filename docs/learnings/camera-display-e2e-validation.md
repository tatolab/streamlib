# Camera-display E2E validation without physical hardware

## When you need this

You changed anything in the GPU pipeline (`vulkan_device.rs`, `vulkan_buffer.rs`,
`vulkan_texture.rs`, `runtime/streamlib-media-builtins/src/camera_source.rs`,
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
2. Boots `examples/camera-display` with `streamlib run` — the app is Python, so
   there is no build step between an edit and the run
3. Waits for the node to register, then asserts against `streamlib graph`:
   both native built-ins present, linked camera → window
4. Captures the window to PNG, then stops the node with SIGTERM and requires a
   clean exit
5. Gates the log on `OUT_OF_DEVICE_MEMORY` / `DEVICE_LOST` / `process() failed`

Exit codes: 0 = pass, 1 = fail, 77 = skipped (prerequisites missing).

## Assert on contracts, not on tracing prose

> ~~Validates: DMA-BUF pools created, swapchain created, first frame captured.~~
> — Superseded 2026-08-25. The fixture used to grep engine log prose
> (`Ring textures created`, `First frame captured`, `Failed to create camera
> texture`). Those strings were renamed or deleted during the pivot and the
> gate went vacuous — it would have reported FAIL on a perfectly healthy run,
> and nobody noticed because the example it built had stopped compiling too.

Gate on things the plan makes durable: the `graph` tool's JSON shape, the JSONL
log schema, process exit status, and the pixels in a captured PNG. Vulkan error
strings (`OUT_OF_DEVICE_MEMORY`, `DEVICE_LOST`) are also stable — they come from
the driver, not from us. Our own `tracing` messages are not a test API.

## AI-tappable validation

The window capture lands in `$OUTPUT_DIR/png_samples/window.png`, grabbed with
`xdotool search --name` → `xwd` → PIL (`tests/fixtures/capture_window.py`).
Read it with the Read tool and describe what it shows.

Because it is a capture of the composited window, it validates the *whole* path
including the swapchain present — which the retired in-process PNG sampler did
not (it dumped the source HOST_VISIBLE pixel buffer before rendering).

> ~~Debug env vars read by display.rs: `STREAMLIB_DISPLAY_FRAME_LIMIT`,
> `STREAMLIB_DISPLAY_PNG_SAMPLE_DIR`, `STREAMLIB_DISPLAY_PNG_SAMPLE_EVERY`.~~
> — Removed 2026-08-25: the display built-in reads none of them; they went with
> the pre-pivot display processor. A run self-terminates on SIGTERM (`rt.run()`
> owns it) and frames are sampled by capturing the window from outside.
> `STREAMLIB_CAMERA_DEVICE` is unaffected — it is read by `examples/camera-display`'s
> own `app.py`, not by the engine.

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
- The app under test: `examples/camera-display/app.py`
