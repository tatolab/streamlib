# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""A Rive animation as a stream source.

rive-runtime's Vulkan renderer draws each frame straight into the node's output
texture, on a device this wheel's Rust creates on the runtime's GPU. Nothing
here links the engine.
"""

from .rive_source import RiveSource as RiveSource
from .rive_source import RiveSourceConfig as RiveSourceConfig

__all__ = ["RiveSource", "RiveSourceConfig"]
