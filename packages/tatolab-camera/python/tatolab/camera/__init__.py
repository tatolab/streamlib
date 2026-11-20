# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""A camera node as an extension wheel.

Capture runs in this wheel's own Rust — V4L2 on Linux, AVFoundation on macOS —
which hands each capture buffer to the engine as a DMA-BUF or an IOSurface and
has it converted to RGBA on the GPU, with no CPU copy. Nothing here links the
engine.
"""

from .camera_source import CameraSource as CameraSource
from .capture_to_downstream_latency_probe import (
    CaptureToDownstreamLatencyProbe as CaptureToDownstreamLatencyProbe,
)

__all__ = [
    "CameraSource",
    "CaptureToDownstreamLatencyProbe",
]
