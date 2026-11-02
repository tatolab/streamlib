# tatolab-camera-proof

A Linux camera node built the way a third party would build one, on today's
public surface: a standalone maturin wheel that depends on `tatolab-stream`
and `pyo3`, and on no engine crate.

- `V4l2CameraSource` opens a V4L2 device in its own processor interpreter,
  captures NV12 or YUYV on a thread in this wheel's Rust, and in `process()`
  uploads the newest frame through a host-visible pixel buffer, copies it into
  a texture, and converts it to RGBA with this wheel's own GLSL kernel.
- `CaptureToDownstreamLatencyProbe` logs the frame rate and the
  capture-to-read latency of the frames it receives.

The CPU upload is deliberate: this proves the module shape, not the
performance.

## Run it

With this machine's runtime running:

```bash
cd streams/module-camera      # or streams/built-in-camera for the baseline
tatolab dev
```

Both streams expose H.264 at `http://127.0.0.1:4446/watch/<machine>/main`, and
both log a `CaptureToDownstreamLatencyProbe[...]` report every 600 frames.
Only one can hold the camera at a time.

## Check it

```bash
cargo fmt --all --check && cargo test --locked --lib && cargo clippy --locked --lib --all-targets -- -D warnings
uv venv --python 3.12 .venv && VIRTUAL_ENV=.venv uv pip install ../../sdk/tatolab-stream mypy
VIRTUAL_ENV=.venv uvx maturin@1.9.6 develop
.venv/bin/python -m mypy.stubtest tatolab.camera_proof._native && uvx pyright@1.1.411
```
