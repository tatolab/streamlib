# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""H.264 encode and decode for StreamLib, on a Vulkan device the module owns.

An extension wheel: the Rust is inside this package and the node functions
below are the binding. Nothing here links the engine — the wheel depends on
`tatolab-stream`, and the node runs in its own node interpreter like
any other Python node.
"""

from .nodes import H264Decoder as H264Decoder
from .nodes import H264Encoder as H264Encoder

__all__ = ["H264Decoder", "H264Encoder"]
