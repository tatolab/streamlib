"""
GPU context for managing WebGPU device and queue.

This module provides a high-level context for GPU operations,
abstracting the underlying WebGPU backend.
"""

from typing import Optional, Dict, Any
from .backends.webgpu import WebGPUBackend

try:
    import wgpu
    HAS_WGPU = True
except ImportError:
    HAS_WGPU = False


class GPUContext:
    """
    GPU context for a StreamRuntime.

    Manages WebGPU device, queue, and provides high-level GPU operations.
    Each runtime should have its own GPU context (not global).

    Example:
        # Create context
        gpu_ctx = await GPUContext.create()

        # Get device info
        print(f"Using {gpu_ctx.backend_name} on {gpu_ctx.device_name}")

        # Create buffer
        buffer = gpu_ctx.create_buffer(size=1920*1080*3)
    """

    def __init__(self, backend: WebGPUBackend):
        """
        Initialize GPU context (use create() instead).

        Args:
            backend: WebGPU backend instance
        """
        self.backend = backend
        self._memory_pool: Dict[tuple, list] = {}  # (size, usage) -> [buffer, ...]

    @classmethod
    async def create(
        cls,
        power_preference: str = 'high-performance'
    ) -> 'GPUContext':
        """
        Create GPU context (async).

        Args:
            power_preference: 'high-performance' or 'low-power'

        Returns:
            GPUContext instance

        Raises:
            RuntimeError: If WebGPU not available

        Example:
            gpu_ctx = await GPUContext.create()
        """
        if not HAS_WGPU:
            raise RuntimeError(
                "WebGPU not available. Install with: pip install wgpu"
            )

        # Create WebGPU backend
        backend = await WebGPUBackend.create(power_preference=power_preference)

        return cls(backend=backend)

    @property
    def device(self) -> 'wgpu.GPUDevice':
        """Get WebGPU device."""
        return self.backend.device

    @property
    def queue(self) -> 'wgpu.GPUQueue':
        """Get WebGPU command queue."""
        return self.backend.queue

    @property
    def adapter(self) -> 'wgpu.GPUAdapter':
        """Get WebGPU adapter."""
        return self.backend.adapter

    @property
    def backend_name(self) -> str:
        """
        Get backend name.

        Returns:
            'Metal' (macOS), 'D3D12' (Windows), or 'Vulkan' (Linux)
        """
        return self.backend.backend_name

    @property
    def device_name(self) -> str:
        """
        Get device name.

        Returns:
            GPU device name (e.g., "Apple M1 Pro", "NVIDIA RTX 4090")
        """
        info = self.backend.adapter_info
        return info.get('description', 'Unknown GPU')

    @property
    def limits(self) -> Dict[str, int]:
        """
        Get device limits.

        Returns:
            Dictionary with device limits
        """
        return self.backend.limits

    def create_buffer(
        self,
        size: int,
        usage: Optional[int] = None,
        label: Optional[str] = None
    ) -> 'wgpu.GPUBuffer':
        """
        Create GPU buffer.

        Args:
            size: Buffer size in bytes
            usage: Buffer usage flags (default: STORAGE | COPY_DST | COPY_SRC)
            label: Optional debug label

        Returns:
            WebGPU buffer

        Example:
            buffer = gpu_ctx.create_buffer(size=1920*1080*3)
        """
        if usage is None:
            usage = (
                wgpu.BufferUsage.STORAGE |
                wgpu.BufferUsage.COPY_DST |
                wgpu.BufferUsage.COPY_SRC
            )

        return self.backend.create_buffer(size=size, usage=usage, label=label)

    def create_texture(
        self,
        width: int,
        height: int,
        format: str = 'rgba8unorm',
        usage: Optional[int] = None,
        label: Optional[str] = None
    ) -> 'wgpu.GPUTexture':
        """
        Create GPU texture.

        Args:
            width: Texture width in pixels
            height: Texture height in pixels
            format: Texture format (default: 'rgba8unorm')
            usage: Texture usage flags (default: TEXTURE_BINDING | COPY_DST | COPY_SRC)
            label: Optional debug label

        Returns:
            WebGPU texture

        Example:
            texture = gpu_ctx.create_texture(width=1920, height=1080)
        """
        return self.backend.create_texture(
            width=width,
            height=height,
            format=format,
            usage=usage,
            label=label
        )

    def allocate_buffer(self, size: int, usage: int) -> 'wgpu.GPUBuffer':
        """
        Allocate buffer from memory pool (reuse if available).

        Args:
            size: Buffer size in bytes
            usage: Buffer usage flags

        Returns:
            WebGPU buffer (reused or newly allocated)
        """
        key = (size, usage)

        # Try to reuse from pool
        if key in self._memory_pool and len(self._memory_pool[key]) > 0:
            return self._memory_pool[key].pop()

        # Allocate new buffer
        return self.create_buffer(size=size, usage=usage)

    def release_buffer(self, buffer: 'wgpu.GPUBuffer', size: int, usage: int) -> None:
        """
        Return buffer to memory pool for reuse.

        Args:
            buffer: Buffer to release
            size: Buffer size in bytes
            usage: Buffer usage flags
        """
        key = (size, usage)

        if key not in self._memory_pool:
            self._memory_pool[key] = []

        # Add to pool (limit pool size to avoid memory bloat)
        if len(self._memory_pool[key]) < 10:
            self._memory_pool[key].append(buffer)

    def clear_memory_pool(self) -> None:
        """Clear all buffers from memory pool."""
        self._memory_pool.clear()

    def __repr__(self) -> str:
        return (
            f"GPUContext(backend={self.backend_name}, "
            f"device={self.device_name})"
        )
