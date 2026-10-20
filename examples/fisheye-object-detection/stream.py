# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""A Tatolab stream: camera → a fisheye lens → rectify → detect, both views shared.

`tatolab dev` loads the one `@stream` below by convention — there is no
manifest. `CameraSource` and the two `H264Encoder`s are native built-ins; the
lens and the detector are this project's own nodes, each running in its own
child interpreter. While it runs, http://127.0.0.1:4446/watch/<machine>/main
plays the stream in a browser, with both encoded tracks to pick from.

Nodes live in their own modules under `nodes/`, never in this file: each one
runs in its own child interpreter, which imports the class by name.
"""

from tatolab.stream import (
    CameraSource,
    CameraSourceConfig,
    Exposure,
    H264Encoder,
    StreamBuilder,
    stream,
)

from nodes.radial_distortion_model import (
    DEFAULT_RADIAL_DISTORTION_K1,
    DEFAULT_RADIAL_DISTORTION_K2,
)
from nodes.synthetic_fisheye_lens import SyntheticFisheyeLens
from nodes.undistorting_object_detector import UndistortingObjectDetector

# The capture device: a V4L2 path (`/dev/video2`) on Linux, an AVFoundation
# unique ID on macOS. `None` opens the first capture device found.
CAMERA_DEVICE_ID: str | None = None

FRAME_WIDTH = 1280
FRAME_HEIGHT = 720

DETECTION_CONFIDENCE_THRESHOLD = 0.35


@stream
def main(stream_builder: StreamBuilder) -> None:
    """The camera through a synthetic fisheye lens, rectified and detected; both views encoded and exposed."""
    camera_config: CameraSourceConfig = {
        "max_width": FRAME_WIDTH,
        "max_height": FRAME_HEIGHT,
    }
    if CAMERA_DEVICE_ID is not None:
        camera_config["device_id"] = CAMERA_DEVICE_ID
    camera = stream_builder.add(CameraSource, config=camera_config)
    lens = stream_builder.add(
        SyntheticFisheyeLens,
        config={
            "radial_distortion_k1": DEFAULT_RADIAL_DISTORTION_K1,
            "radial_distortion_k2": DEFAULT_RADIAL_DISTORTION_K2,
        },
    )
    detector = stream_builder.add(
        UndistortingObjectDetector,
        config={
            "radial_distortion_k1": DEFAULT_RADIAL_DISTORTION_K1,
            "radial_distortion_k2": DEFAULT_RADIAL_DISTORTION_K2,
            "detection_confidence_threshold": DETECTION_CONFIDENCE_THRESHOLD,
        },
    )
    fisheye_frame_encoder = stream_builder.add(
        H264Encoder, name="fisheye-frame-encoder"
    )
    annotated_frame_encoder = stream_builder.add(
        H264Encoder, name="annotated-frame-encoder"
    )

    stream_builder.connect(
        camera.output("video"), lens.input("camera_frame_from_upstream")
    )
    # One output port, two destinations: the encoder that shows the distorted
    # frame and the detector that rectifies it.
    stream_builder.connect(
        lens.output("fisheye_frame_to_downstream"),
        fisheye_frame_encoder.input("video"),
    )
    stream_builder.connect(
        lens.output("fisheye_frame_to_downstream"),
        detector.input("fisheye_frame_from_upstream"),
    )
    stream_builder.connect(
        detector.output("annotated_frame_to_downstream"),
        annotated_frame_encoder.input("video"),
    )
    stream_builder.expose(
        fisheye_frame_encoder.output("encoded_video"), Exposure.PRIVATE
    )
    stream_builder.expose(
        annotated_frame_encoder.output("encoded_video"), Exposure.PRIVATE
    )
