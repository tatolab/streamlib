# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""The `@graph_generator` functions `test_camera_source.py` runs from the suite project."""

from tatolab.camera import CameraSource
from tatolab.stream import StreamBuilder, graph_generator

#: A device id no capture backend on any machine opens.
UNOPENABLE_DEVICE_ID = "/dev/video-not-a-real-camera"


@graph_generator
def a_camera_source_alone(stream_builder: StreamBuilder) -> None:
    stream_builder.add(CameraSource)


@graph_generator
def a_camera_naming_a_device_no_backend_can_open(stream_builder: StreamBuilder) -> None:
    stream_builder.add(CameraSource, config={"device_id": UNOPENABLE_DEVICE_ID})
