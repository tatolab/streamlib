# Generated from com.tatolab.encodedvideoframe@1.0.0
# DO NOT EDIT - regenerate with `streamlib schema sync`

from dataclasses import dataclass
from typing import Optional, Any

"""Encoded video frame with compressed NAL unit data."""
@dataclass
class Encodedvideoframe:
    """Encoded NAL units (H.264/H.265 bitstream data)."""
    data: bytes
    """Monotonic timestamp in nanoseconds."""
    timestamp_ns: int
    """Whether this is a keyframe (I-frame)."""
    is_keyframe: bool
    """Sequential frame number."""
    frame_number: int

