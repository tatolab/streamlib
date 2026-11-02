# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The baseline: the built-in `CameraSource` → H.264, exposed on this machine, with the same latency probe.

The same graph as `../module-camera` with the engine's own camera in place of
this wheel's, so the two reports compare one device under one probe.
"""

from tatolab.stream import (
    CameraSource,
    CameraSourceConfig,
    Exposure,
    H264Encoder,
    StreamBuilder,
    stream,
)

from tatolab.camera import CaptureToDownstreamLatencyProbe

# A V4L2 node such as `/dev/video0`; `None` opens the first capture device found.
CAMERA_DEVICE_PATH: str | None = None


@stream
def main(stream_builder: StreamBuilder) -> None:
    """The built-in camera, encoded and exposed; frame rate and latency logged."""
    camera_config: CameraSourceConfig = {"max_width": 1920, "max_height": 1080}
    if CAMERA_DEVICE_PATH is not None:
        camera_config["device_id"] = CAMERA_DEVICE_PATH
    camera = stream_builder.add(CameraSource, config=camera_config)
    probe = stream_builder.add(
        CaptureToDownstreamLatencyProbe, config={"camera_label": "built-in"}
    )
    encoder = stream_builder.add(H264Encoder)
    stream_builder.connect(camera.output("video"), probe.input("video_from_upstream"))
    stream_builder.connect(camera.output("video"), encoder.input("video"))
    stream_builder.expose(encoder.output("encoded_video"), Exposure.PRIVATE)
