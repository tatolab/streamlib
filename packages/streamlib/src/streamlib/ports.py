"""
WebGPU-first ports for StreamHandler inputs/outputs.

All ports operate on WebGPU textures. No CPU fallback options.
This design ensures zero-copy GPU pipelines throughout.

Follows the WebGPU-first architecture: simple, opinionated, GPU-only.
"""

from typing import Optional
from .buffers import RingBuffer


class StreamOutput:
    """
    Output port for sending WebGPU data.

    Ports are WebGPU-only - all data must be GPU textures.
    Runtime provides shared GPU context for texture management.

    Example:
        # All outputs are WebGPU (the only option)
        self.outputs['video'] = StreamOutput('video', port_type='video')
    """

    def __init__(
        self,
        name: str,
        port_type: str,  # 'video', 'audio', 'data'
        slots: int = 3
    ):
        """
        Initialize output port.

        Args:
            name: Port name (e.g., 'video', 'audio', 'out')
            port_type: Port type ('video', 'audio', 'data')
            slots: Ring buffer size (default: 3, broadcast practice)
        """
        self.name = name
        self.port_type = port_type
        self.buffer = RingBuffer(slots=slots)

    def write(self, data) -> None:
        """
        Write data to ring buffer (zero-copy reference).

        Args:
            data: Data to write (VideoFrame, AudioBuffer, etc.)
        """
        self.buffer.write(data)

    def __repr__(self) -> str:
        return f"StreamOutput(name='{self.name}', type='{self.port_type}')"


class StreamInput:
    """
    Input port for receiving WebGPU data.

    Ports are WebGPU-only - expects all data as GPU textures.
    Runtime provides shared GPU context for texture management.

    Example:
        # All inputs are WebGPU (the only option)
        self.inputs['video'] = StreamInput('video', port_type='video')
    """

    def __init__(
        self,
        name: str,
        port_type: str  # 'video', 'audio', 'data'
    ):
        """
        Initialize input port.

        Args:
            name: Port name (e.g., 'video', 'audio', 'in')
            port_type: Port type ('video', 'audio', 'data')
        """
        self.name = name
        self.port_type = port_type
        self.buffer: Optional[RingBuffer] = None

    def connect(self, buffer: RingBuffer) -> None:
        """
        Connect to upstream ring buffer.

        Args:
            buffer: Ring buffer from upstream output port

        Note: This is called by StreamRuntime.connect()
        """
        self.buffer = buffer

    def read_latest(self):
        """
        Read latest data from ring buffer (zero-copy reference).

        Returns:
            Latest data (VideoFrame, AudioBuffer, etc.), or None if no data yet

        Note: Returns reference to data in ring buffer, not a copy.
        """
        if self.buffer is None:
            return None
        return self.buffer.read_latest()

    def is_connected(self) -> bool:
        """Check if this input is connected to an upstream output."""
        return self.buffer is not None

    def __repr__(self) -> str:
        return f"StreamInput(name='{self.name}', type='{self.port_type}')"


# Typed port helpers (WebGPU-only)

def VideoOutput(name: str, slots: int = 3) -> StreamOutput:
    """
    Helper to create a video output port (WebGPU-only).

    Args:
        name: Port name (e.g., 'video', 'out')
        slots: Ring buffer size (default: 3)

    Returns:
        StreamOutput configured for video

    Example:
        self.outputs['video'] = VideoOutput('video')  # WebGPU textures only
    """
    return StreamOutput(name, port_type='video', slots=slots)


def VideoInput(name: str) -> StreamInput:
    """
    Helper to create a video input port (WebGPU-only).

    Args:
        name: Port name (e.g., 'video', 'in')

    Returns:
        StreamInput configured for video

    Example:
        self.inputs['video'] = VideoInput('video')  # WebGPU textures only
    """
    return StreamInput(name, port_type='video')


def AudioOutput(name: str, slots: int = 3) -> StreamOutput:
    """
    Helper to create an audio output port (WebGPU-only).

    Args:
        name: Port name (e.g., 'audio', 'out')
        slots: Ring buffer size (default: 3)

    Returns:
        StreamOutput configured for audio

    Example:
        self.outputs['audio'] = AudioOutput('audio')  # WebGPU buffers only
    """
    return StreamOutput(name, port_type='audio', slots=slots)


def AudioInput(name: str) -> StreamInput:
    """
    Helper to create an audio input port (WebGPU-only).

    Args:
        name: Port name (e.g., 'audio', 'in')

    Returns:
        StreamInput configured for audio

    Example:
        self.inputs['audio'] = AudioInput('audio')  # WebGPU buffers only
    """
    return StreamInput(name, port_type='audio')


def DataOutput(name: str, slots: int = 3) -> StreamOutput:
    """
    Helper to create a generic data output port (WebGPU-only).

    Args:
        name: Port name (e.g., 'data', 'out')
        slots: Ring buffer size (default: 3)

    Returns:
        StreamOutput configured for data

    Example:
        self.outputs['data'] = DataOutput('data')  # WebGPU buffers only
    """
    return StreamOutput(name, port_type='data', slots=slots)


def DataInput(name: str) -> StreamInput:
    """
    Helper to create a generic data input port (WebGPU-only).

    Args:
        name: Port name (e.g., 'data', 'in')

    Returns:
        StreamInput configured for data

    Example:
        self.inputs['data'] = DataInput('data')  # WebGPU buffers only
    """
    return StreamInput(name, port_type='data')
