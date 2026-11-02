# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""H.265 encode for StreamLib, on a Vulkan device the module owns.

An extension wheel: the Rust is inside this package and the `@node` class below
is the binding. Nothing here links the engine — the wheel depends on
`tatolab-stream`, and the processor runs in its own processor interpreter like
any other Python processor.
"""

from .processors import H265Encoder as H265Encoder
from .processors import H265EncoderConfig as H265EncoderConfig

__all__ = ["H265Encoder", "H265EncoderConfig"]
