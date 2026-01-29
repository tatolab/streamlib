# Generated from com.tatolab.audioframe@1.0.0
# DO NOT EDIT - regenerate with `streamlib schema sync`

from dataclasses import dataclass
from typing import Optional, Any

"""Audio frame with interleaved samples for IPC transfer."""
@dataclass
class Audioframe:
    """Interleaved audio samples as little-endian f32 bytes."""
    samples: bytes
    """Number of audio channels (1-8)."""
    channels: int
    """Sample rate in Hz (e.g., 44100, 48000)."""
    sample_rate: int
    """Monotonic timestamp in nanoseconds."""
    timestamp_ns: int
    """Sequential frame number."""
    frame_number: int

