"""
Capability-based ports for StreamHandler inputs/outputs.

Ports declare what memory spaces they can work with ('cpu', 'gpu', or both).
Runtime negotiates capabilities when connecting handlers and auto-inserts
transfer handlers when memory spaces don't overlap.

This design is inspired by GStreamer's capability negotiation system.
"""

from typing import List, Optional
from .buffers import RingBuffer


class StreamOutput:
    """
    Output port with capability negotiation.

    Capabilities list memory spaces this port can produce:
    - ['cpu'] - CPU memory only (numpy arrays)
    - ['gpu'] - GPU memory only (torch tensors)
    - ['cpu', 'gpu'] - Flexible, can produce either

    Example:
        # CPU-only output
        self.outputs['video'] = StreamOutput('video', port_type='video', capabilities=['cpu'])

        # GPU-only output
        self.outputs['video'] = StreamOutput('video', port_type='video', capabilities=['gpu'])

        # Flexible output (can produce either)
        self.outputs['video'] = StreamOutput('video', port_type='video', capabilities=['cpu', 'gpu'])
    """

    def __init__(
        self,
        name: str,
        port_type: str,  # 'video', 'audio', 'data'
        capabilities: List[str],  # ['cpu'], ['gpu'], or ['cpu', 'gpu']
        slots: int = 3
    ):
        """
        Initialize output port.

        Args:
            name: Port name (e.g., 'video', 'audio', 'out')
            port_type: Port type ('video', 'audio', 'data')
            capabilities: List of supported memory spaces
            slots: Ring buffer size (default: 3, broadcast practice)
        """
        if not capabilities:
            raise ValueError("Output port must declare at least one capability")

        valid_capabilities = {'cpu', 'gpu', 'metal'}
        for cap in capabilities:
            if cap not in valid_capabilities:
                raise ValueError(f"Invalid capability '{cap}'. Must be 'cpu', 'gpu', or 'metal'")

        self.name = name
        self.port_type = port_type
        self.capabilities = capabilities
        self.buffer = RingBuffer(slots=slots)
        self.negotiated_memory: Optional[str] = None  # Set during runtime.connect()

    def write(self, data) -> None:
        """
        Write data to ring buffer (zero-copy reference).

        Args:
            data: Data to write (VideoFrame, AudioBuffer, etc.)
        """
        self.buffer.write(data)

    def __repr__(self) -> str:
        return f"StreamOutput(name='{self.name}', type='{self.port_type}', caps={self.capabilities})"


class StreamInput:
    """
    Input port with capability negotiation.

    Capabilities list memory spaces this port can accept:
    - ['cpu'] - CPU memory only
    - ['gpu'] - GPU memory only
    - ['cpu', 'gpu'] - Flexible, can accept either

    Example:
        # CPU-only input
        self.inputs['video'] = StreamInput('video', port_type='video', capabilities=['cpu'])

        # GPU-only input
        self.inputs['video'] = StreamInput('video', port_type='video', capabilities=['gpu'])

        # Flexible input (can accept either)
        self.inputs['video'] = StreamInput('video', port_type='video', capabilities=['cpu', 'gpu'])
    """

    def __init__(
        self,
        name: str,
        port_type: str,  # 'video', 'audio', 'data'
        capabilities: List[str]  # ['cpu'], ['gpu'], or ['cpu', 'gpu']
    ):
        """
        Initialize input port.

        Args:
            name: Port name (e.g., 'video', 'audio', 'in')
            port_type: Port type ('video', 'audio', 'data')
            capabilities: List of supported memory spaces
        """
        if not capabilities:
            raise ValueError("Input port must declare at least one capability")

        valid_capabilities = {'cpu', 'gpu', 'metal'}
        for cap in capabilities:
            if cap not in valid_capabilities:
                raise ValueError(f"Invalid capability '{cap}'. Must be 'cpu', 'gpu', or 'metal'")

        self.name = name
        self.port_type = port_type
        self.capabilities = capabilities
        self.buffer: Optional[RingBuffer] = None  # Connected during runtime.connect()
        self.negotiated_memory: Optional[str] = None  # Set during runtime.connect()

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
        return f"StreamInput(name='{self.name}', type='{self.port_type}', caps={self.capabilities})"


# Typed port helpers for common use cases

def VideoOutput(name: str, capabilities: Optional[List[str]] = None, slots: int = 3) -> StreamOutput:
    """
    Helper to create a video output port.

    Args:
        name: Port name (e.g., 'video', 'out')
        capabilities: ['cpu'], ['gpu'], or ['cpu', 'gpu']. Defaults to ['gpu'] (GPU-first)
        slots: Ring buffer size (default: 3)

    Returns:
        StreamOutput configured for video

    Example:
        self.outputs['video'] = VideoOutput('video')  # GPU by default
    """
    if capabilities is None:
        capabilities = ['gpu']  # GPU-first by default
    return StreamOutput(name, port_type='video', capabilities=capabilities, slots=slots)


def VideoInput(name: str, capabilities: Optional[List[str]] = None) -> StreamInput:
    """
    Helper to create a video input port.

    Args:
        name: Port name (e.g., 'video', 'in')
        capabilities: ['cpu'], ['gpu'], or ['cpu', 'gpu']. Defaults to ['gpu'] (GPU-first)

    Returns:
        StreamInput configured for video

    Example:
        self.inputs['video'] = VideoInput('video')  # GPU by default
    """
    if capabilities is None:
        capabilities = ['gpu']  # GPU-first by default
    return StreamInput(name, port_type='video', capabilities=capabilities)


def AudioOutput(name: str, capabilities: Optional[List[str]] = None, slots: int = 3) -> StreamOutput:
    """
    Helper to create an audio output port.

    Args:
        name: Port name (e.g., 'audio', 'out')
        capabilities: ['cpu'], ['gpu'], or ['cpu', 'gpu']. Defaults to ['gpu']
        slots: Ring buffer size (default: 3)

    Returns:
        StreamOutput configured for audio

    Example:
        self.outputs['audio'] = AudioOutput('audio')  # GPU by default
    """
    if capabilities is None:
        capabilities = ['gpu']
    return StreamOutput(name, port_type='audio', capabilities=capabilities, slots=slots)


def AudioInput(name: str, capabilities: Optional[List[str]] = None) -> StreamInput:
    """
    Helper to create an audio input port.

    Args:
        name: Port name (e.g., 'audio', 'in')
        capabilities: ['cpu'], ['gpu'], or ['cpu', 'gpu']. Defaults to ['gpu']

    Returns:
        StreamInput configured for audio

    Example:
        self.inputs['audio'] = AudioInput('audio')  # GPU by default
    """
    if capabilities is None:
        capabilities = ['gpu']
    return StreamInput(name, port_type='audio', capabilities=capabilities)


def DataOutput(name: str, capabilities: Optional[List[str]] = None, slots: int = 3) -> StreamOutput:
    """
    Helper to create a generic data output port.

    Args:
        name: Port name (e.g., 'data', 'out')
        capabilities: ['cpu'], ['gpu'], or ['cpu', 'gpu']. Defaults to ['gpu']
        slots: Ring buffer size (default: 3)

    Returns:
        StreamOutput configured for data

    Example:
        self.outputs['data'] = DataOutput('data')  # GPU by default
    """
    if capabilities is None:
        capabilities = ['gpu']
    return StreamOutput(name, port_type='data', capabilities=capabilities, slots=slots)


def DataInput(name: str, capabilities: Optional[List[str]] = None) -> StreamInput:
    """
    Helper to create a generic data input port.

    Args:
        name: Port name (e.g., 'data', 'in')
        capabilities: ['cpu'], ['gpu'], or ['cpu', 'gpu']. Defaults to ['gpu']

    Returns:
        StreamInput configured for data

    Example:
        self.inputs['data'] = DataInput('data')  # GPU by default
    """
    if capabilities is None:
        capabilities = ['gpu']
    return StreamInput(name, port_type='data', capabilities=capabilities)
