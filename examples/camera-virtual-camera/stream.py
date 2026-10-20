# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""A Tatolab stream: one camera in, two virtual cameras out.

`tatolab dev` loads the one `@stream` below by convention — there is no
manifest. While it runs, every other application on the machine sees two extra
cameras in its picker: one showing the capture device untouched, one showing
the same picture with its colours inverted by a Python node.

Nodes live in their own modules, never in this file: each one runs in its own
child interpreter, which imports the class by name.
"""

from typing import Literal

from tatolab.stream import (
    CameraSource,
    CameraSourceConfig,
    StreamBuilder,
    VirtualCameraSink,
    VirtualCameraSinkConfig,
    stream,
)

from nodes.inverting_effect import InvertingEffect

# The capture device: a V4L2 path (`/dev/video2`) on Linux. `None` opens the
# first capture device found.
CAMERA_DEVICE_ID: str | None = None

# Each camera's name in every picker. The loopback door's label holds 31 bytes,
# so a longer name is cut to fit.
PASSTHROUGH_VIRTUAL_CAMERA_NAME = "Tatolab Camera"
INVERTED_VIRTUAL_CAMERA_NAME = "Tatolab Camera Inverted"

# Both sinks' door: `auto` takes the v4l2loopback door where its control node is
# writable and the PipeWire door otherwise.
VIRTUAL_CAMERA_DOOR: Literal["auto", "v4l2loopback", "pipewire"] = "auto"


@stream
def main(stream_builder: StreamBuilder) -> None:
    """The camera fanned to a passthrough virtual camera and, through the effect, to an inverted one."""
    camera_config: CameraSourceConfig = {}
    if CAMERA_DEVICE_ID is not None:
        camera_config["device_id"] = CAMERA_DEVICE_ID
    camera = stream_builder.add(CameraSource, config=camera_config)
    inverting_effect = stream_builder.add(InvertingEffect)
    passthrough_virtual_camera_config: VirtualCameraSinkConfig = {
        "name": PASSTHROUGH_VIRTUAL_CAMERA_NAME,
        "door": VIRTUAL_CAMERA_DOOR,
    }
    passthrough_virtual_camera = stream_builder.add(
        VirtualCameraSink,
        name="passthrough-virtual-camera",
        config=passthrough_virtual_camera_config,
    )
    inverted_virtual_camera_config: VirtualCameraSinkConfig = {
        "name": INVERTED_VIRTUAL_CAMERA_NAME,
        "door": VIRTUAL_CAMERA_DOOR,
    }
    inverted_virtual_camera = stream_builder.add(
        VirtualCameraSink,
        name="inverted-virtual-camera",
        config=inverted_virtual_camera_config,
    )

    stream_builder.connect(
        camera.output("video"), passthrough_virtual_camera.input("video")
    )
    stream_builder.connect(
        camera.output("video"), inverting_effect.input("video_from_upstream")
    )
    stream_builder.connect(
        inverting_effect.output("video_to_downstream"),
        inverted_virtual_camera.input("video"),
    )
