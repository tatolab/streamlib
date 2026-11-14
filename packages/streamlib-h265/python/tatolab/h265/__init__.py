# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: GPL-3.0-only

"""H.265 encode and decode for StreamLib, on a Vulkan device the module owns.

An extension wheel: the Rust is inside this package and the `@node.process` classes
below are the binding. Nothing here links the engine — the wheel depends on
`tatolab-stream`, and the processor runs in its own processor interpreter like
any other Python processor.
"""

from .processors import H265Decoder as H265Decoder
from .processors import H265DecoderConfig as H265DecoderConfig
from .processors import H265Encoder as H265Encoder
from .processors import H265EncoderConfig as H265EncoderConfig

__all__ = ["H265Decoder", "H265DecoderConfig", "H265Encoder", "H265EncoderConfig"]
