# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""H.264 encode for StreamLib, on a Vulkan device the module owns.

An extension wheel: the Rust is inside this package and the `@node` class below
is the binding. Nothing here links the engine — the wheel depends on
`tatolab-stream`, and the processor runs in its own processor interpreter like
any other Python processor.
"""

from .processors import H264Encoder as H264Encoder
from .processors import H264EncoderConfig as H264EncoderConfig

__all__ = ["H264Encoder", "H264EncoderConfig"]
