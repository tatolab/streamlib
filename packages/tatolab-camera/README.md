# tatolab-camera

A camera node built the way a third party builds one: a standalone maturin
wheel that depends on `tatolab-stream`, `pyo3` and `tatolab-module-sdk`, and on
no engine crate.

- `CameraSource` opens a V4L2 device in its own processor interpreter. On
  `start()` it lends its `video` output to this wheel's Rust, which exports each
  driver buffer as a DMA-BUF (`VIDIOC_EXPBUF`) and adopts it once through the
  module host table, with its NV12 or YUYV plane layout. Per frame, a thread
  here takes a texture from the node's output pool, has the engine convert the
  filled buffer into it on the GPU, and publishes a `VideoFrame` naming it. No
  frame is copied on the CPU, and Python never runs at frame cadence.
- `CaptureToDownstreamLatencyProbe` logs the frame rate and the
  capture-to-read latency of the frames it receives.

Linux only for now; the macOS half imports IOSurfaces through the same
conversion.

## Run it

With this machine's runtime running:

```bash
cd streams/module-camera      # or streams/built-in-camera for the baseline
tatolab dev
```

Both streams expose H.264 at `http://127.0.0.1:4446/watch/<machine>/main`, and
both log a `CaptureToDownstreamLatencyProbe[...]` report every 600 frames; the
module's capture thread logs its own capture-to-publish latency as often. Only
one can hold the camera at a time.

## Check it

```bash
cargo fmt --all --check && cargo test --locked --lib && cargo clippy --locked --lib --all-targets -- -D warnings
uv venv --python 3.12 .venv && VIRTUAL_ENV=.venv uv pip install ../../sdk/tatolab-stream mypy
VIRTUAL_ENV=.venv uvx maturin@1.9.6 develop
.venv/bin/python -m mypy.stubtest tatolab.camera._native && uvx pyright@1.1.411
```
