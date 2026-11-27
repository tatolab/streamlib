# tatolab-camera

A camera node built the way a third party builds one: a standalone maturin
wheel that depends on `tatolab-stream`, `pyo3` and `tatolab-module-sdk`, and on
no engine crate.

- On macOS, camera access is asked for only by a `CameraSource`, and only in a
  stream someone loads or starts: it asks without waiting, and frames begin once
  the user allows it. A stream the runtime re-loads on its own as it starts never
  asks, and its `CameraSource` fails to open the camera until access was answered.
  A stream with no camera node never touches AVFoundation.
- `CaptureToDownstreamLatencyProbe` logs the frame rate and the
  capture-to-read latency of the frames it receives.

## Run it

With this machine's runtime running:

```bash
cd streams/module-camera
tatolab dev
```

The camera's `video` is seen in Tato Studio's viewer, and the stream logs a
`CaptureToDownstreamLatencyProbe[...]` report every 600 frames; the capture
thread logs its own capture-to-publish latency as often.

## Check it

```bash
cargo fmt --all --check && cargo test --locked --lib && cargo clippy --locked --lib --all-targets -- -D warnings
uv venv --python 3.12 .venv && VIRTUAL_ENV=.venv uv pip install ../../sdk/tatolab-stream mypy
VIRTUAL_ENV=.venv uvx maturin@1.9.6 develop
.venv/bin/python -m mypy.stubtest tatolab.camera._native && uvx pyright@1.1.411
```
