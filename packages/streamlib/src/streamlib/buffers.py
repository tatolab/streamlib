"""
Ring buffers for zero-copy data exchange between actors.

This module provides fixed-size circular buffers with latest-read semantics,
matching professional broadcast practice (SMPTE ST 2110).
"""

import threading
from typing import Any, Optional, Tuple, TYPE_CHECKING, TypeVar, Generic

if TYPE_CHECKING:
    import torch

try:
    import torch
    TORCH_AVAILABLE = True
except ImportError:
    TORCH_AVAILABLE = False


T = TypeVar('T')


class RingBuffer(Generic[T]):
    """
    Fixed-size circular buffer for CPU data.

    Key properties:
    - Fixed memory allocation (3 slots by default)
    - Latest-read semantics (skip old data)
    - Overwrite oldest slot when full
    - Thread-safe (uses lock)
    - No queueing, no backpressure

    This matches professional broadcast practice where old frames are
    worthless and should be discarded in favor of new ones.
    """

    def __init__(self, slots: int = 3):
        """
        Initialize ring buffer.

        Args:
            slots: Number of buffer slots (default: 3, matches broadcast practice)
        """
        if slots < 1:
            raise ValueError(f"Ring buffer must have at least 1 slot, got {slots}")

        self.slots = slots
        self.buffer = [None] * slots
        self.write_idx = 0
        self.lock = threading.Lock()
        self.has_data = False

    def write(self, data: T) -> None:
        """
        Write data to ring buffer, overwriting oldest slot.

        Args:
            data: Data to write
        """
        with self.lock:
            self.buffer[self.write_idx] = data
            self.write_idx = (self.write_idx + 1) % self.slots
            self.has_data = True

    def read_latest(self) -> Optional[T]:
        """
        Read most recent data from ring buffer.

        Returns:
            Most recent data, or None if no data written yet
        """
        with self.lock:
            if not self.has_data:
                return None
            idx = (self.write_idx - 1) % self.slots
            return self.buffer[idx]

    def is_empty(self) -> bool:
        """
        Check if any data has been written.

        Returns:
            True if no data written yet, False otherwise
        """
        with self.lock:
            return not self.has_data

    def clear(self) -> None:
        """Clear buffer (reset to empty state)."""
        with self.lock:
            self.buffer = [None] * self.slots
            self.write_idx = 0
            self.has_data = False


class GPURingBuffer:
    """
    Zero-copy GPU memory ring buffer using PyTorch.

    Key properties:
    - Pre-allocated GPU memory (no runtime allocation)
    - Zero-copy: Data stays on GPU
    - Latest-read semantics
    - Thread-safe
    - Efficient for GPU-to-GPU transfers

    Usage:
        buffer = GPURingBuffer(slots=3, shape=(1920, 1080, 3))

        # Write side (producer)
        write_buf = buffer.get_write_buffer()
        # ... fill write_buf directly on GPU ...
        buffer.advance()

        # Read side (consumer)
        read_buf = buffer.get_read_buffer()
        # ... use read_buf directly on GPU ...
    """

    def __init__(
        self,
        slots: int = 3,
        shape: Tuple[int, ...] = (1920, 1080, 3),
        device: str = 'cuda',
        dtype = None  # torch.uint8 if torch available
    ):
        """
        Initialize GPU ring buffer.

        Args:
            slots: Number of buffer slots (default: 3)
            shape: Shape of each buffer (e.g., (H, W, C) for images)
            device: PyTorch device ('cuda', 'cuda:0', etc.)
            dtype: PyTorch dtype (default: uint8 for images)

        Raises:
            RuntimeError: If PyTorch not available or CUDA not available
        """
        if not TORCH_AVAILABLE:
            raise RuntimeError("PyTorch not available. Install with: pip install torch")

        if not torch.cuda.is_available():
            raise RuntimeError("CUDA not available. GPU ring buffer requires CUDA.")

        if slots < 1:
            raise ValueError(f"Ring buffer must have at least 1 slot, got {slots}")

        self.slots = slots
        self.shape = shape
        self.device = torch.device(device)
        self.dtype = dtype if dtype is not None else torch.uint8

        # Pre-allocate GPU buffers
        self.buffers = [
            torch.zeros(shape, device=self.device, dtype=self.dtype)
            for _ in range(slots)
        ]

        self.write_idx = 0
        self.lock = threading.Lock()
        self.has_data = False

    def get_write_buffer(self) -> 'torch.Tensor':
        """
        Get GPU buffer to write into (zero-copy).

        Returns:
            PyTorch tensor on GPU for writing

        Usage:
            buf = ring_buffer.get_write_buffer()
            # Fill buf directly (e.g., from GPU operation)
            buf[:] = some_gpu_operation()
            ring_buffer.advance()
        """
        with self.lock:
            return self.buffers[self.write_idx]

    def advance(self) -> None:
        """
        Mark current write buffer as ready and advance to next slot.

        Call this after filling the write buffer via get_write_buffer().
        """
        with self.lock:
            self.write_idx = (self.write_idx + 1) % self.slots
            self.has_data = True

    def get_read_buffer(self) -> Optional['torch.Tensor']:
        """
        Get latest GPU buffer for reading (zero-copy).

        Returns:
            Most recent PyTorch tensor on GPU, or None if no data yet

        Usage:
            buf = ring_buffer.get_read_buffer()
            if buf is not None:
                result = some_gpu_operation(buf)
        """
        with self.lock:
            if not self.has_data:
                return None
            idx = (self.write_idx - 1) % self.slots
            return self.buffers[idx]

    def is_empty(self) -> bool:
        """
        Check if any data has been written.

        Returns:
            True if no data written yet, False otherwise
        """
        with self.lock:
            return not self.has_data

    def clear(self) -> None:
        """
        Clear buffer (reset to empty state).

        Note: Does not deallocate GPU memory, just resets write index.
        """
        with self.lock:
            self.write_idx = 0
            self.has_data = False

    def to_cpu(self) -> Optional[Any]:
        """
        Read latest buffer and transfer to CPU.

        Returns:
            NumPy array on CPU, or None if no data

        Note: This is a convenience method. For zero-copy GPU workflows,
        use get_read_buffer() directly.
        """
        buf = self.get_read_buffer()
        if buf is None:
            return None
        return buf.cpu().numpy()
