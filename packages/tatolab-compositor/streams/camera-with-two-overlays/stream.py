# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""A 1080p camera with two overlays composited over it, encoded and exposed, with the composite's frame rate logged.

Watch it at http://127.0.0.1:4446/watch/<machine>/main. The camera wheel's
`CaptureToDownstreamLatencyProbe` after the compositor reports the composite's
frame rate and its capture-to-read latency in the project's log.
"""

from tatolab.stream import Exposure, StreamBuilder, stream

from tatolab.camera import CameraSource, CaptureToDownstreamLatencyProbe
from tatolab.compositor import AlphaOverCompositor
from tatolab.h264 import H264Encoder
from tatolab.testpattern import TestPatternSource

# A V4L2 node such as `/dev/video0`; `None` opens the first capture device found.
CAMERA_DEVICE_PATH: str | None = None

CANVAS_WIDTH = 1920
CANVAS_HEIGHT = 1080


@stream
def main(stream_builder: StreamBuilder) -> None:
    """The camera on `layer_0`, a picture-in-picture and a lower-third band over it."""
    camera_config: dict[str, object] = {"max_width": CANVAS_WIDTH, "max_height": CANVAS_HEIGHT}
    if CAMERA_DEVICE_PATH is not None:
        camera_config["device_path"] = CAMERA_DEVICE_PATH
    camera = stream_builder.add(CameraSource, config=camera_config)
    picture_in_picture = stream_builder.add(
        TestPatternSource, config={"width": 640, "height": 360}
    )
    lower_third = stream_builder.add(TestPatternSource, config={"width": 1280, "height": 160})
    compositor = stream_builder.add(
        AlphaOverCompositor,
        config={
            "width": CANVAS_WIDTH,
            "height": CANVAS_HEIGHT,
            "layers": [
                {},
                {"x": 1240, "y": 40, "opacity": 1.0},
                {"x": 80, "y": 860, "opacity": 0.75},
            ],
        },
    )
    stream_builder.connect(camera.output("video"), compositor.input("layer_0"))
    stream_builder.connect(picture_in_picture.output("video"), compositor.input("layer_1"))
    stream_builder.connect(lower_third.output("video"), compositor.input("layer_2"))

    probe = stream_builder.add(
        CaptureToDownstreamLatencyProbe, config={"camera_label": "composited"}
    )
    encoder = stream_builder.add(H264Encoder)
    stream_builder.connect(compositor.output("video"), probe.input("video_from_upstream"))
    stream_builder.connect(compositor.output("video"), encoder.input("video"))
    stream_builder.expose(encoder.output("encoded_video"), Exposure.PRIVATE)
