# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""H.265 encode and decode for StreamLib, on a Vulkan device the module owns.

An extension wheel: the Rust is inside this package and the node functions
below are the binding. Nothing here links the engine — the wheel depends on
`tatolab-stream`, and the node runs in its own node interpreter like
any other Python node.
"""

from ._native import VideoCodingSessionRefused as VideoCodingSessionRefused
from ._native import VideoCodingSessionRefusedForNow as VideoCodingSessionRefusedForNow
from ._native import VideoCodingSessionUnanswered as VideoCodingSessionUnanswered
from ._native import VideoCodingThreadEnded as VideoCodingThreadEnded
from ._native import VideoCodingThreadNotStarted as VideoCodingThreadNotStarted
from .nodes import DecodedPictureBufferBoundSetAlone as DecodedPictureBufferBoundSetAlone
from .nodes import H265Decoder as H265Decoder
from .nodes import H265Encoder as H265Encoder

__all__ = [
    "DecodedPictureBufferBoundSetAlone",
    "H265Decoder",
    "H265Encoder",
    "VideoCodingSessionRefused",
    "VideoCodingSessionRefusedForNow",
    "VideoCodingSessionUnanswered",
    "VideoCodingThreadEnded",
    "VideoCodingThreadNotStarted",
]
