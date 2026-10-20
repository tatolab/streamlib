# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""A Tatolab stream: camera → a halftone compute kernel → H.264, shared on this machine.

`tatolab dev` loads the one `@stream` below by convention — there is no
manifest. `CameraSource` and `H264Encoder` are native built-ins; the effect
between them is this project's own node, and the GLSL it dispatches is a
string in that module, compiled by the engine when the node sets up.

Nodes live in their own modules, never in this file: each one runs in its
own child interpreter, which imports the class by name.
"""

from tatolab.stream import (
    CameraSource,
    CameraSourceConfig,
    Exposure,
    H264Encoder,
    StreamBuilder,
    stream,
)

from nodes.halftone_compute import HalftoneCompute

# The capture device: a V4L2 path (`/dev/video2`) on Linux, an AVFoundation
# unique ID on macOS. `None` opens the first capture device found.
CAMERA_DEVICE_ID: str | None = None

CAPTURE_MAX_WIDTH = 1920
CAPTURE_MAX_HEIGHT = 1080


@stream
def main(stream_builder: StreamBuilder) -> None:
    """The camera screened into halftone dots, encoded and exposed on this machine."""
    camera_config: CameraSourceConfig = {
        "max_width": CAPTURE_MAX_WIDTH,
        "max_height": CAPTURE_MAX_HEIGHT,
    }
    if CAMERA_DEVICE_ID is not None:
        camera_config["device_id"] = CAMERA_DEVICE_ID
    camera = stream_builder.add(CameraSource, config=camera_config)
    halftone = stream_builder.add(
        HalftoneCompute,
        config={"cell_size": 8, "dot_boost": 1.3, "background_level": 0.0627},
    )
    encoder = stream_builder.add(H264Encoder)
    stream_builder.connect(
        camera.output("video"), halftone.input("camera_frame_from_upstream")
    )
    stream_builder.connect(
        halftone.output("halftone_frame_to_downstream"), encoder.input("video")
    )
    stream_builder.expose(encoder.output("encoded_video"), Exposure.PRIVATE)
