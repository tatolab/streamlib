# tatolab-camera

A camera node built the way a third party builds one: a standalone maturin
wheel that depends on `tatolab-stream`, `pyo3` and `tatolab-module-sdk`, and on
no engine crate.

- `CameraSource` opens a camera in its own processor interpreter. On
  `setup()` it lends its `video` output to this wheel's Rust, which opens the
  camera and adopts each capture buffer once through the module host table:
  - on Linux, each V4L2 driver buffer exported as a DMA-BUF (`VIDIOC_EXPBUF`),
    with its NV12 or YUYV plane layout;
  - on macOS, each IOSurface an AVFoundation capture session delivers, in
    `420v` or `420f`.

  Per frame, the capture thread takes a texture from the node's output pool,
  has the engine convert the filled buffer into it on the GPU, and publishes a
  `VideoFrame` naming it, from `start()` on. No frame is copied on the CPU, and Python never runs
  at frame cadence. `device_id` names a V4L2 node such as `/dev/video0` on
  Linux and an AVFoundation unique ID on macOS.
- On macOS, camera access is asked for only by a `CameraSource`, and only in a
  stream someone loads or starts: it asks without waiting, and frames begin once
  the user allows it. A stream the runtime re-loads on its own as it starts never
  asks, and its `CameraSource` refuses at `setup()` until access was answered.
  A stream with no camera node never touches AVFoundation.
- `CaptureToDownstreamLatencyProbe` logs the frame rate and the
  capture-to-read latency of the frames it receives.

## Run it

With this machine's runtime running:

```bash
cd streams/module-camera
tatolab dev
```

The stream exposes H.264 at `http://127.0.0.1:4446/watch/<machine>/main` and
logs a `CaptureToDownstreamLatencyProbe[...]` report every 600 frames; the
capture thread logs its own capture-to-publish latency as often.

## Check it

```bash
cargo fmt --all --check && cargo test --locked --lib && cargo clippy --locked --lib --all-targets -- -D warnings
uv venv --python 3.12 .venv && VIRTUAL_ENV=.venv uv pip install ../../sdk/tatolab-stream mypy
VIRTUAL_ENV=.venv uvx maturin@1.9.6 develop
.venv/bin/python -m mypy.stubtest tatolab.camera._native && uvx pyright@1.1.411
```
