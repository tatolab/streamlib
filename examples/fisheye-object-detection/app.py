# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1
"""A StreamLib app: camera → a fisheye lens → rectify → detect → two windows.

The pipeline a monocular drone flies with, on a desk. A wide-FOV lens barrels
the periphery of every frame, which is exactly where a COCO-trained detector
is weakest, so the frame is rectified on the GPU before it reaches the model.
Here the lens is synthetic — `SyntheticFisheyeLens` warps an ordinary webcam
so there is something to rectify — and both windows are open at once so the
distortion and its correction are visible side by side.

`streamlib run` finds `setup(rt)` below by convention — there is no manifest
and no `main()`. `CameraSource` and `DisplayWindow` are native built-ins that
ship inside the wheel; the two processors between them are this app's own
Python, each running in its own child interpreter.
"""

import os

from processors.synthetic_fisheye_lens import SyntheticFisheyeLens
from processors.undistorting_object_detector import UndistortingObjectDetector

from streamlib import CameraSource, DisplayWindow, Runtime

FRAME_WIDTH = 1280
FRAME_HEIGHT = 720

# Polynomial radial-distortion coefficients, shared by the lens that applies
# them and the rectifier that inverts them. A real lens's pair comes out of a
# checkerboard calibration and is a property of the hardware; a synthetic lens
# is the one case where the rectifier can be handed the exact numbers, which
# is what makes this app a clean demonstration rather than a calibration
# exercise. Negative `k1` is the barrel direction — content is pushed outward
# and the frame curves in at the edges.
RADIAL_DISTORTION_K1 = -0.25
RADIAL_DISTORTION_K2 = 0.0


def setup(rt: Runtime) -> None:
    camera_configuration: dict[str, object] = {
        "max_width": FRAME_WIDTH,
        "max_height": FRAME_HEIGHT,
    }
    # Unset means "the first capture device the engine finds"; set it to point
    # this app at a particular node, a vivid virtual camera included.
    requested_camera_device = os.environ.get("STREAMLIB_CAMERA_DEVICE")
    if requested_camera_device:
        camera_configuration["device_id"] = requested_camera_device

    camera = rt.add(CameraSource, config=camera_configuration)
    lens = rt.add(
        SyntheticFisheyeLens,
        config={
            "radial_distortion_k1": RADIAL_DISTORTION_K1,
            "radial_distortion_k2": RADIAL_DISTORTION_K2,
        },
    )
    detector = rt.add(
        UndistortingObjectDetector,
        config={
            "radial_distortion_k1": RADIAL_DISTORTION_K1,
            "radial_distortion_k2": RADIAL_DISTORTION_K2,
            "detection_confidence_threshold": 0.35,
        },
    )
    lens_window = rt.add(
        DisplayWindow,
        config={
            "title": "Fisheye lens — what the drone sees",
            "width": FRAME_WIDTH,
            "height": FRAME_HEIGHT,
            "scaling": "fit",
        },
        display_name="LensWindow",
    )
    detection_window = rt.add(
        DisplayWindow,
        config={
            "title": "Rectified + detections",
            "width": FRAME_WIDTH,
            "height": FRAME_HEIGHT,
            "scaling": "fit",
        },
        display_name="DetectionWindow",
    )

    rt.connect(camera.output("video"), lens.input("camera_frame_from_upstream"))
    # One output port, two destinations: the window that shows the distorted
    # frame and the detector that rectifies it. The engine sizes the channel
    # for both — nothing about the producer changes.
    rt.connect(
        lens.output("fisheye_frame_to_downstream"), lens_window.input("video")
    )
    rt.connect(
        lens.output("fisheye_frame_to_downstream"),
        detector.input("fisheye_frame_from_upstream"),
    )
    rt.connect(
        detector.output("annotated_frame_to_downstream"),
        detection_window.input("video"),
    )
