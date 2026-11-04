# tatolab-virtualcamera

A virtual camera for Tatolab streams, built the way a third party builds a node:
an extension wheel that depends on `tatolab-stream` and on no engine crate, its
native half reaching the engine's PipeWire camera door through the module host
table. Linux only.

- `VirtualCameraSink` presents each frame on `video` as the picture of a
  camera any Linux application can select. Each instance is one
  camera, created at setup and removed at teardown, like a USB camera plugged in
  and pulled out.
  - Config: `name`, the camera's name in every picker, at most 31 bytes;
    absent, `StreamLib Camera` plus four characters stable across runs of the
    same stream and node. `door`: `v4l2loopback`, a `/dev/video*` camera every
    application sees; `pipewire`, a PipeWire camera portal-based applications
    select, with no kernel module and no grant; `auto` (the default), the
    loopback camera where it is permitted and PipeWire otherwise.
  - The engine converts each RGBA frame into YUYV on the GPU; the camera's
    format follows the first frame's extent and colour, and an extent change
    re-negotiates it. A frame of odd width drops every later frame.
  - Without write access to `/dev/v4l2loopback` the loopback door refuses,
    naming `tatolab enable-virtual-camera`, the one-time command that loads the
    module and grants it behind your desktop's password prompt. Without a
    PipeWire daemon the PipeWire door refuses naming why.

```python
from tatolab.virtualcamera import VirtualCameraSink

camera = stream_builder.add(VirtualCameraSink, config={"name": "Desk Camera"})
```

## Check it

```bash
uv venv --python 3.12 .venv && VIRTUAL_ENV=.venv uv pip install ../../sdk/tatolab-stream pytest mypy
VIRTUAL_ENV=.venv uvx maturin@1.9.6 develop
.venv/bin/python -m mypy.stubtest tatolab.virtualcamera._native
.venv/bin/pytest tests && uvx pyright@1.1.411
```

The camera tests run on a runtime with the module loaded, in the integration
suite's `test_virtual_camera_sink.py`.
