# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""A Linux camera built the way a third party would build one.

V4L2 capture runs on a thread in this wheel's own Rust; each frame is uploaded
through the CPU door the Python surface offers and converted to RGBA by this
wheel's own GLSL kernel. Nothing here links the engine.
"""

from .camera_source import V4l2CameraSource as V4l2CameraSource
from .camera_source import V4l2CameraSourceConfig as V4l2CameraSourceConfig
from .capture_to_downstream_latency_probe import (
    CaptureToDownstreamLatencyProbe as CaptureToDownstreamLatencyProbe,
)
from .capture_to_downstream_latency_probe import (
    CaptureToDownstreamLatencyProbeConfig as CaptureToDownstreamLatencyProbeConfig,
)

__all__ = [
    "CaptureToDownstreamLatencyProbe",
    "CaptureToDownstreamLatencyProbeConfig",
    "V4l2CameraSource",
    "V4l2CameraSourceConfig",
]
