# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""A Tatolab stream: camera → a GLSL compute kernel → H.264, shared on this machine.

`tatolab dev` loads the one `@stream` below by convention — there is no
manifest. `CameraSource` and `H264Encoder` are native built-ins; the effect
between them is this project's own Python, and the GLSL it dispatches is a
string in that module, so there is no shader toolchain to install.

Nodes live in their own modules, never in this file: each one runs in its own
child interpreter, which imports the class by name.
"""

from tatolab.stream import (
    CameraSource,
    CameraSourceConfig,
    Exposure,
    H264Encoder,
    StreamBuilder,
    stream,
)

from nodes.grayscale_compute import GrayscaleCompute

# The capture device: a V4L2 path (`/dev/video2`) on Linux, an AVFoundation
# unique ID on macOS. `None` opens the first capture device found.
CAMERA_DEVICE_ID: str | None = None


@stream
def main(stream_builder: StreamBuilder) -> None:
    """The camera graded to black and white by a compute kernel, encoded and exposed on this machine."""
    camera_config: CameraSourceConfig = {"max_width": 1920, "max_height": 1080}
    if CAMERA_DEVICE_ID is not None:
        camera_config["device_id"] = CAMERA_DEVICE_ID
    camera = stream_builder.add(CameraSource, config=camera_config)
    grayscale = stream_builder.add(GrayscaleCompute, config={"strength": 1.0})
    encoder = stream_builder.add(H264Encoder)
    stream_builder.connect(
        camera.output("video"), grayscale.input("camera_frame_from_upstream")
    )
    stream_builder.connect(
        grayscale.output("grayscale_frame_to_downstream"), encoder.input("video")
    )
    stream_builder.expose(encoder.output("encoded_video"), Exposure.PRIVATE)
