"""
Message types for inter-actor communication.

Messages are stored in ring buffers and passed between actors.
All messages include timing information for synchronization.

Message types:
- VideoFrame: Video frame data (RGB images)
- AudioBuffer: Audio sample buffer (PCM audio)
- KeyEvent: Keyboard input event
- MouseEvent: Mouse input event
- DataMessage: Generic data message

Note: Messages are just data structures. They don't carry behavior.
"""

import numpy as np
from dataclasses import dataclass
from typing import Optional, Any, Dict


@dataclass
class VideoFrame:
    """
    Video frame message.

    Attributes:
        data: Frame data as NumPy array, shape (H, W, 3), dtype uint8, RGB format
        timestamp: Absolute timestamp in seconds (from tick)
        frame_number: Monotonic frame counter (from tick)
        width: Frame width in pixels
        height: Frame height in pixels
        metadata: Optional metadata dict (codec info, etc.)
    """
    data: np.ndarray
    timestamp: float
    frame_number: int
    width: int
    height: int
    metadata: Optional[Dict[str, Any]] = None

    def __post_init__(self):
        """Validate frame data."""
        if self.data.ndim != 3 or self.data.shape[2] != 3:
            raise ValueError(
                f"VideoFrame data must be (H, W, 3), got {self.data.shape}"
            )
        if self.data.dtype != np.uint8:
            raise ValueError(
                f"VideoFrame data must be uint8, got {self.data.dtype}"
            )
        # Validate dimensions match
        h, w = self.data.shape[:2]
        if h != self.height or w != self.width:
            raise ValueError(
                f"VideoFrame dimensions mismatch: data {(w, h)} != specified ({self.width}, {self.height})"
            )


@dataclass
class AudioBuffer:
    """
    Audio buffer message.

    Attributes:
        data: Audio samples as NumPy array, shape (samples, channels), dtype float32
        timestamp: Absolute timestamp in seconds (from tick)
        sample_rate: Sample rate in Hz (e.g., 48000)
        channels: Number of channels (1=mono, 2=stereo)
        metadata: Optional metadata dict (codec info, etc.)
    """
    data: np.ndarray
    timestamp: float
    sample_rate: int
    channels: int
    metadata: Optional[Dict[str, Any]] = None

    def __post_init__(self):
        """Validate audio data."""
        if self.data.ndim != 2:
            raise ValueError(
                f"AudioBuffer data must be 2D (samples, channels), got {self.data.ndim}D"
            )
        if self.data.dtype != np.float32:
            raise ValueError(
                f"AudioBuffer data must be float32, got {self.data.dtype}"
            )
        # Validate channels match
        if self.data.shape[1] != self.channels:
            raise ValueError(
                f"AudioBuffer channels mismatch: data has {self.data.shape[1]}, specified {self.channels}"
            )

    @property
    def duration(self) -> float:
        """Get duration in seconds."""
        return self.data.shape[0] / self.sample_rate


@dataclass
class KeyEvent:
    """
    Keyboard event message.

    Attributes:
        key: Key code or character
        pressed: True if pressed, False if released
        timestamp: Event timestamp
        modifiers: Modifier keys (shift, ctrl, alt, etc.)
    """
    key: str
    pressed: bool
    timestamp: float
    modifiers: Optional[Dict[str, bool]] = None


@dataclass
class MouseEvent:
    """
    Mouse event message.

    Attributes:
        x: X coordinate
        y: Y coordinate
        button: Button number (0=left, 1=middle, 2=right, -1=move)
        pressed: True if pressed, False if released (None for move)
        timestamp: Event timestamp
    """
    x: int
    y: int
    button: int
    pressed: Optional[bool]
    timestamp: float


@dataclass
class DataMessage:
    """
    Generic data message.

    Used for arbitrary data that doesn't fit other message types.

    Attributes:
        data: Arbitrary data (any Python object)
        timestamp: Message timestamp
        data_type: Type hint (string, e.g., 'json', 'binary', 'custom')
        metadata: Optional metadata dict
    """
    data: Any
    timestamp: float
    data_type: str = 'unknown'
    metadata: Optional[Dict[str, Any]] = None
