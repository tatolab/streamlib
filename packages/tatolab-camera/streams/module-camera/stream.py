# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The camera as a module: `tatolab.camera.CameraSource`, seen in a viewer, with a latency probe.

Watch the camera's `video` in Tato Studio's viewer. The probe's reports land
in the project's log.
"""

from tatolab.stream import StreamBuilder, stream

from tatolab.camera import CameraSource, CaptureToDownstreamLatencyProbe

# A V4L2 node such as `/dev/video0` on Linux, an AVFoundation unique ID on macOS;
# `None` opens the first camera found.
CAMERA_DEVICE_ID: str | None = None


@stream
def main(stream_builder: StreamBuilder) -> None:
    """The camera through this module, frame rate and latency logged."""
    camera_config: dict[str, object] = {"max_width": 1920, "max_height": 1080}
    if CAMERA_DEVICE_ID is not None:
        camera_config["device_id"] = CAMERA_DEVICE_ID
    camera = stream_builder.add(CameraSource, config=camera_config)
    probe = stream_builder.add(
        CaptureToDownstreamLatencyProbe, config={"camera_label": "module"}
    )
    stream_builder.connect(camera.output("video"), probe.input("video_from_upstream"))
