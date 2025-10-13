"""
GPU-first ports for StreamHandler inputs/outputs.

Ports are GPU by default. Runtime automatically handles memory management.
CPU is only used when explicitly requested (rare).

This design follows the docs-first architecture: simple, opinionated, GPU-first.
"""

from typing import Optional
from .buffers import RingBuffer


class StreamOutput:
    """
    Output port for sending data (GPU by default).

    Ports are GPU-first - data stays on GPU unless explicitly configured otherwise.
    Runtime automatically manages memory without manual capability negotiation.

    Example:
        # GPU output (default - recommended)
        self.outputs['video'] = StreamOutput('video', port_type='video')

        # CPU-only output (rare - legacy compatibility)
        self.outputs['video'] = StreamOutput('video', port_type='video', cpu_only=True)

        # Flexible output (can produce either GPU or CPU)
        self.outputs['video'] = StreamOutput('video', port_type='video', allow_cpu=True)
    """

    def __init__(
        self,
        name: str,
        port_type: str,  # 'video', 'audio', 'data'
        allow_cpu: bool = False,  # Can fall back to CPU if needed
        cpu_only: bool = False,   # Force CPU (rare, for legacy)
        slots: int = 3
    ):
        """
        Initialize output port.

        Args:
            name: Port name (e.g., 'video', 'audio', 'out')
            port_type: Port type ('video', 'audio', 'data')
            allow_cpu: Allow CPU fallback if GPU unavailable (default: False)
            cpu_only: Force CPU-only operation (rare, default: False)
            slots: Ring buffer size (default: 3, broadcast practice)
        """
        if cpu_only and allow_cpu:
            raise ValueError("Cannot set both cpu_only=True and allow_cpu=True")

        self.name = name
        self.port_type = port_type
        self.allow_cpu = allow_cpu
        self.cpu_only = cpu_only
        self.buffer = RingBuffer(slots=slots)

    def write(self, data) -> None:
        """
        Write data to ring buffer (zero-copy reference).

        Args:
            data: Data to write (VideoFrame, AudioBuffer, etc.)
        """
        self.buffer.write(data)

    def is_gpu(self) -> bool:
        """Check if this port operates on GPU."""
        return not self.cpu_only

    def __repr__(self) -> str:
        memory = "CPU-only" if self.cpu_only else ("GPU+CPU" if self.allow_cpu else "GPU")
        return f"StreamOutput(name='{self.name}', type='{self.port_type}', memory={memory})"


class StreamInput:
    """
    Input port for receiving data (GPU by default).

    Ports are GPU-first - expects data on GPU unless explicitly configured otherwise.
    Runtime automatically manages memory without manual capability negotiation.

    Example:
        # GPU input (default - recommended)
        self.inputs['video'] = StreamInput('video', port_type='video')

        # CPU-only input (rare - legacy compatibility)
        self.inputs['video'] = StreamInput('video', port_type='video', cpu_only=True)

        # Flexible input (can accept either GPU or CPU)
        self.inputs['video'] = StreamInput('video', port_type='video', allow_cpu=True)
    """

    def __init__(
        self,
        name: str,
        port_type: str,  # 'video', 'audio', 'data'
        allow_cpu: bool = False,  # Can accept CPU if needed
        cpu_only: bool = False    # Force CPU (rare, for legacy)
    ):
        """
        Initialize input port.

        Args:
            name: Port name (e.g., 'video', 'audio', 'in')
            port_type: Port type ('video', 'audio', 'data')
            allow_cpu: Allow CPU fallback if GPU unavailable (default: False)
            cpu_only: Force CPU-only operation (rare, default: False)
        """
        if cpu_only and allow_cpu:
            raise ValueError("Cannot set both cpu_only=True and allow_cpu=True")

        self.name = name
        self.port_type = port_type
        self.allow_cpu = allow_cpu
        self.cpu_only = cpu_only
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

    def is_gpu(self) -> bool:
        """Check if this port operates on GPU."""
        return not self.cpu_only

    def __repr__(self) -> str:
        memory = "CPU-only" if self.cpu_only else ("GPU+CPU" if self.allow_cpu else "GPU")
        return f"StreamInput(name='{self.name}', type='{self.port_type}', memory={memory})"


# Typed port helpers (GPU by default)

def VideoOutput(name: str, allow_cpu: bool = False, cpu_only: bool = False, slots: int = 3) -> StreamOutput:
    """
    Helper to create a video output port (GPU by default).

    Args:
        name: Port name (e.g., 'video', 'out')
        allow_cpu: Allow CPU fallback (default: False)
        cpu_only: Force CPU-only (rare, default: False)
        slots: Ring buffer size (default: 3)

    Returns:
        StreamOutput configured for video

    Example:
        self.outputs['video'] = VideoOutput('video')  # GPU by default
    """
    return StreamOutput(name, port_type='video', allow_cpu=allow_cpu, cpu_only=cpu_only, slots=slots)


def VideoInput(name: str, allow_cpu: bool = False, cpu_only: bool = False) -> StreamInput:
    """
    Helper to create a video input port (GPU by default).

    Args:
        name: Port name (e.g., 'video', 'in')
        allow_cpu: Allow CPU fallback (default: False)
        cpu_only: Force CPU-only (rare, default: False)

    Returns:
        StreamInput configured for video

    Example:
        self.inputs['video'] = VideoInput('video')  # GPU by default
    """
    return StreamInput(name, port_type='video', allow_cpu=allow_cpu, cpu_only=cpu_only)


def AudioOutput(name: str, allow_cpu: bool = False, cpu_only: bool = False, slots: int = 3) -> StreamOutput:
    """
    Helper to create an audio output port (GPU by default).

    Args:
        name: Port name (e.g., 'audio', 'out')
        allow_cpu: Allow CPU fallback (default: False)
        cpu_only: Force CPU-only (rare, default: False)
        slots: Ring buffer size (default: 3)

    Returns:
        StreamOutput configured for audio

    Example:
        self.outputs['audio'] = AudioOutput('audio')  # GPU by default
    """
    return StreamOutput(name, port_type='audio', allow_cpu=allow_cpu, cpu_only=cpu_only, slots=slots)


def AudioInput(name: str, allow_cpu: bool = False, cpu_only: bool = False) -> StreamInput:
    """
    Helper to create an audio input port (GPU by default).

    Args:
        name: Port name (e.g., 'audio', 'in')
        allow_cpu: Allow CPU fallback (default: False)
        cpu_only: Force CPU-only (rare, default: False)

    Returns:
        StreamInput configured for audio

    Example:
        self.inputs['audio'] = AudioInput('audio')  # GPU by default
    """
    return StreamInput(name, port_type='audio', allow_cpu=allow_cpu, cpu_only=cpu_only)


def DataOutput(name: str, allow_cpu: bool = False, cpu_only: bool = False, slots: int = 3) -> StreamOutput:
    """
    Helper to create a generic data output port (GPU by default).

    Args:
        name: Port name (e.g., 'data', 'out')
        allow_cpu: Allow CPU fallback (default: False)
        cpu_only: Force CPU-only (rare, default: False)
        slots: Ring buffer size (default: 3)

    Returns:
        StreamOutput configured for data

    Example:
        self.outputs['data'] = DataOutput('data')  # GPU by default
    """
    return StreamOutput(name, port_type='data', allow_cpu=allow_cpu, cpu_only=cpu_only, slots=slots)


def DataInput(name: str, allow_cpu: bool = False, cpu_only: bool = False) -> StreamInput:
    """
    Helper to create a generic data input port (GPU by default).

    Args:
        name: Port name (e.g., 'data', 'in')
        allow_cpu: Allow CPU fallback (default: False)
        cpu_only: Force CPU-only (rare, default: False)

    Returns:
        StreamInput configured for data

    Example:
        self.inputs['data'] = DataInput('data')  # GPU by default
    """
    return StreamInput(name, port_type='data', allow_cpu=allow_cpu, cpu_only=cpu_only)
