# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The `@stream` functions `test_camera_source.py` runs from the suite project."""

import tatolab.stream
from tatolab.stream import StreamBuilder, stream

#: A device id no capture backend on any machine opens.
UNOPENABLE_DEVICE_ID = "/dev/video-not-a-real-camera"


@stream
def a_camera_source_alone(stream_builder: StreamBuilder) -> None:
    stream_builder.add(tatolab.stream.CameraSource)


@stream
def a_camera_naming_a_device_no_backend_can_open(stream_builder: StreamBuilder) -> None:
    stream_builder.add(tatolab.stream.CameraSource, config={"device_id": UNOPENABLE_DEVICE_ID})
