# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1
"""A StreamLib app: camera → window.

`streamlib run` finds `setup(rt)` below by convention — there is no manifest and
no `main()`. Both processors are native built-ins that ship inside the wheel, so
this app declares no processor of its own: the pipeline is two `rt.add` calls
and one `rt.connect`.
"""

import os

from streamlib import CameraSource, DisplayWindow, Runtime


def setup(rt: Runtime) -> None:
    camera_configuration: dict[str, object] = {}
    # Unset means "the first capture device the engine finds". The E2E fixture
    # sets it to point this app at a vivid virtual camera instead.
    requested_camera_device = os.environ.get("STREAMLIB_CAMERA_DEVICE")
    if requested_camera_device:
        camera_configuration["device_id"] = requested_camera_device

    camera = rt.add(CameraSource, config=camera_configuration)
    window = rt.add(
        DisplayWindow,
        config={
            "title": "StreamLib Camera Display",
            "width": 1920,
            "height": 1080,
            "scaling": "fit",
        },
    )

    rt.connect(camera.output("video"), window.input("video"))
