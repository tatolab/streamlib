# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""A camera node as an extension wheel.

V4L2 capture runs on a thread in this wheel's own Rust, which hands each driver
buffer to the engine as a DMA-BUF and has it converted to RGBA on the GPU, with
no CPU copy. Nothing here links the engine.
"""

from .camera_source import CameraSource as CameraSource
from .camera_source import CameraSourceConfig as CameraSourceConfig
from .capture_to_downstream_latency_probe import (
    CaptureToDownstreamLatencyProbe as CaptureToDownstreamLatencyProbe,
)
from .capture_to_downstream_latency_probe import (
    CaptureToDownstreamLatencyProbeConfig as CaptureToDownstreamLatencyProbeConfig,
)

__all__ = [
    "CameraSource",
    "CameraSourceConfig",
    "CaptureToDownstreamLatencyProbe",
    "CaptureToDownstreamLatencyProbeConfig",
]
