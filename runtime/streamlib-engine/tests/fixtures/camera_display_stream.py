# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The camera-display rig's stream: a camera into a window."""

import os

from tatolab.stream import CameraSource, DisplayWindow, Stream, stream


@stream
def main(stream: Stream) -> None:
    """A camera into a window — `STREAMLIB_CAMERA_DEVICE` names the camera, else the first found."""
    camera_configuration: dict[str, object] = {}
    requested_camera_device = os.environ.get("STREAMLIB_CAMERA_DEVICE")
    if requested_camera_device:
        camera_configuration["device_id"] = requested_camera_device

    camera = stream.add(CameraSource, config=camera_configuration)
    window = stream.add(
        DisplayWindow,
        config={
            "title": "StreamLib Camera Display",
            "width": 1920,
            "height": 1080,
            "scaling": "fit",
        },
    )
    stream.connect(camera.output("video"), window.input("video"))
