# tatolab-virtualcamera

A virtual camera for Tatolab streams, built the way a third party builds a node:
an ordinary Python package that depends on `tatolab-stream` and on no engine
crate. Linux only.

- `VirtualCameraSink` presents each frame on `video` as the picture of a
  v4l2loopback camera any Linux application can select. Each instance is one
  camera, created at setup and removed at teardown, like a USB camera plugged in
  and pulled out.
  - Config: `name`, the camera's name in every picker, at most 31 bytes;
    absent, `StreamLib Camera` plus four characters stable across runs of the
    same stream and node. `door`: `auto` (the default) and `v4l2loopback` take
    the loopback camera; `pipewire` is refused.
  - The engine converts each RGBA frame into YUYV on the GPU; the camera's
    format follows the first frame's extent and colour, and an extent change
    re-negotiates it. A frame of odd width drops every later frame.
  - Without write access to `/dev/v4l2loopback` setup refuses, naming
    `tatolab enable-virtual-camera`, the one-time command that loads the module
    and grants it behind your desktop's password prompt.

```python
from tatolab.virtualcamera import VirtualCameraSink

camera = stream_builder.add(VirtualCameraSink, config={"name": "Desk Camera"})
```

## Check it

```bash
uv venv --python 3.12 .venv && VIRTUAL_ENV=.venv uv pip install ../../sdk/tatolab-stream -e . pytest
.venv/bin/pytest tests && uvx pyright@1.1.411
```

The camera tests run on a runtime with the module loaded, in the integration
suite's `test_virtual_camera_sink.py`.
