# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1
#
# Generated from com.tatolab.videoframe@1.0.0
# DO NOT EDIT - regenerate with `streamlib schema sync`

from dataclasses import dataclass
from typing import Optional, Any

"""Video frame with GPU surface reference for IPC transfer."""
@dataclass
class Videoframe:
    """Surface Store ticket ID for GPU buffer exchange."""
    surface_id: int
    """Frame width in pixels."""
    width: int
    """Frame height in pixels."""
    height: int
    """Pixel format name (e.g., Bgra32, Nv12)."""
    pixel_format: str
    """Monotonic timestamp in nanoseconds."""
    timestamp_ns: int
    """Sequential frame number."""
    frame_number: int

