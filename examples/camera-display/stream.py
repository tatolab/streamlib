# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""A Tatolab stream: camera → H.264, shared on this machine.

`tatolab dev` loads the one `@stream` below by convention — there is no
manifest. Both nodes are native built-ins, so this stream declares no node of
its own: the pipeline is two `add` calls, one `connect` and one `expose`.
"""

from tatolab.stream import (
    CameraSource,
    CameraSourceConfig,
    Exposure,
    H264Encoder,
    StreamBuilder,
    stream,
)

# The capture device: a V4L2 path (`/dev/video2`) on Linux, an AVFoundation
# unique ID on macOS. `None` opens the first capture device found.
CAMERA_DEVICE_ID: str | None = None


@stream
def main(stream_builder: StreamBuilder) -> None:
    """The camera, encoded and exposed on this machine."""
    camera_config: CameraSourceConfig = {}
    if CAMERA_DEVICE_ID is not None:
        camera_config["device_id"] = CAMERA_DEVICE_ID
    camera = stream_builder.add(CameraSource, config=camera_config)
    encoder = stream_builder.add(H264Encoder)
    stream_builder.connect(camera.output("video"), encoder.input("video"))
    stream_builder.expose(encoder.output("encoded_video"), Exposure.PRIVATE)
