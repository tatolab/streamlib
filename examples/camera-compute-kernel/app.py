# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1
"""A StreamLib app: camera → a GLSL compute kernel → window.

`streamlib run` finds `setup(rt)` below by convention — there is no manifest
and no `main()`. `CameraSource` and `DisplayWindow` are native built-ins that
ship inside the wheel; the effect between them is this app's own Python, and
the GLSL it dispatches is a string in that module — there is no shader
toolchain to install and nothing is compiled ahead of time.

Processors live in their own modules, never in this file: each one runs in its
own child interpreter, which imports the class by name.
"""

import os

from processors.grayscale_compute import GrayscaleCompute

from streamlib import CameraSource, DisplayWindow, Runtime

FRAME_WIDTH = 1920
FRAME_HEIGHT = 1080


def setup(rt: Runtime) -> None:
    camera_configuration: dict[str, object] = {
        "max_width": FRAME_WIDTH,
        "max_height": FRAME_HEIGHT,
    }
    # Unset means "the first capture device the engine finds"; set it to point
    # this app at a particular node, a vivid virtual camera included.
    requested_camera_device = os.environ.get("STREAMLIB_CAMERA_DEVICE")
    if requested_camera_device:
        camera_configuration["device_id"] = requested_camera_device

    camera = rt.add(CameraSource, config=camera_configuration)
    # `strength` is an ordinary constructor keyword with an ordinary Python
    # default — `config` is how a processor's own `__init__` is called, and
    # nothing about the dial is streamlib surface.
    grayscale = rt.add(GrayscaleCompute, config={"strength": 1.0})
    window = rt.add(
        DisplayWindow,
        config={
            "title": "StreamLib Camera Compute Kernel",
            "width": FRAME_WIDTH,
            "height": FRAME_HEIGHT,
            "scaling": "fit",
        },
    )

    rt.connect(camera.output("video"), grayscale.input("camera_frame_from_upstream"))
    rt.connect(
        grayscale.output("grayscale_frame_to_downstream"), window.input("video")
    )
