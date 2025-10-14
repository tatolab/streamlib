"""
GPU buffer and texture wrappers for WebGPU.

Provides high-level abstractions for GPU memory management,
with support for uploading/downloading data to/from GPU.
"""

import numpy as np
from typing import TYPE_CHECKING, Optional

if TYPE_CHECKING:
    import wgpu
    from .context import GPUContext

try:
    import wgpu
    HAS_WGPU = True
except ImportError:
    HAS_WGPU = False


class GPUBuffer:
    """
    GPU buffer wrapper for WebGPU.

    Provides high-level methods for uploading/downloading data to/from GPU.

    Example:
        # Create buffer
        buffer = GPUBuffer.create(gpu_ctx, size=1920*1080*3)

        # Upload numpy array
        frame = np.random.randint(0, 255, (1080, 1920, 3), dtype=np.uint8)
        buffer.write(frame)

        # Download from GPU
        result = buffer.read()
    """

    def __init__(
        self,
        context: 'GPUContext',
        buffer: 'wgpu.GPUBuffer',
        size: int,
        usage: int
    ):
        """
        Initialize GPU buffer (use create() instead).

        Args:
            context: GPU context
            buffer: WebGPU buffer
            size: Buffer size in bytes
            usage: Buffer usage flags
        """
        self.context = context
        self.buffer = buffer
        self.size = size
        self.usage = usage

    @classmethod
    def create(
        cls,
        context: 'GPUContext',
        size: int,
        usage: Optional[int] = None,
        label: Optional[str] = None
    ) -> 'GPUBuffer':
        """
        Create GPU buffer.

        Args:
            context: GPU context
            size: Buffer size in bytes
            usage: Buffer usage flags (default: STORAGE | COPY_DST | COPY_SRC)
            label: Optional debug label

        Returns:
            GPUBuffer instance

        Example:
            buffer = GPUBuffer.create(gpu_ctx, size=1920*1080*3)
        """
        if not HAS_WGPU:
            raise RuntimeError("WebGPU not available")

        if usage is None:
            usage = (
                wgpu.BufferUsage.STORAGE |
                wgpu.BufferUsage.COPY_DST |
                wgpu.BufferUsage.COPY_SRC
            )

        buffer = context.create_buffer(size=size, usage=usage, label=label)

        return cls(context=context, buffer=buffer, size=size, usage=usage)

    def write(self, data: np.ndarray, offset: int = 0) -> None:
        """
        Upload numpy array to GPU buffer.

        Args:
            data: NumPy array to upload
            offset: Byte offset in buffer (default: 0)

        Example:
            frame = np.random.randint(0, 255, (1080, 1920, 3), dtype=np.uint8)
            buffer.write(frame)
        """
        if not isinstance(data, np.ndarray):
            raise TypeError(f"Expected numpy array, got {type(data)}")

        # Ensure contiguous
        if not data.flags['C_CONTIGUOUS']:
            data = np.ascontiguousarray(data)

        # Upload to GPU
        self.context.queue.write_buffer(
            self.buffer,
            offset,
            data
        )

    async def read(self, size: Optional[int] = None, offset: int = 0) -> np.ndarray:
        """
        Download GPU buffer to numpy array (async).

        Args:
            size: Number of bytes to read (default: full buffer)
            offset: Byte offset in buffer (default: 0)

        Returns:
            NumPy array with downloaded data

        Example:
            result = await buffer.read()
        """
        if size is None:
            size = self.size

        # Create staging buffer for readback
        staging_buffer = self.context.create_buffer(
            size=size,
            usage=wgpu.BufferUsage.COPY_DST | wgpu.BufferUsage.MAP_READ
        )

        # Copy GPU buffer → staging buffer
        encoder = self.context.device.create_command_encoder()
        encoder.copy_buffer_to_buffer(
            self.buffer,
            offset,
            staging_buffer,
            0,
            size
        )
        self.context.queue.submit([encoder.finish()])

        # Map staging buffer to CPU
        await staging_buffer.map_async(wgpu.MapMode.READ)

        # Read data
        data_view = staging_buffer.get_mapped_range()
        data = np.frombuffer(data_view, dtype=np.uint8).copy()

        # Unmap
        staging_buffer.unmap()

        return data

    def __repr__(self) -> str:
        return f"GPUBuffer(size={self.size} bytes)"


class GPUTexture:
    """
    GPU texture wrapper for WebGPU.

    Provides high-level methods for uploading/downloading image data to/from GPU.

    Example:
        # Create texture
        texture = GPUTexture.create(gpu_ctx, width=1920, height=1080)

        # Upload numpy array (H, W, 3) or (H, W, 4)
        frame = np.random.randint(0, 255, (1080, 1920, 3), dtype=np.uint8)
        texture.write(frame)

        # Download from GPU
        result = await texture.read()
    """

    def __init__(
        self,
        context: 'GPUContext',
        texture: 'wgpu.GPUTexture',
        width: int,
        height: int,
        format: str
    ):
        """
        Initialize GPU texture (use create() instead).

        Args:
            context: GPU context
            texture: WebGPU texture
            width: Texture width in pixels
            height: Texture height in pixels
            format: Texture format (e.g., 'rgba8unorm')
        """
        self.context = context
        self.texture = texture
        self.width = width
        self.height = height
        self.format = format

    @classmethod
    def create(
        cls,
        context: 'GPUContext',
        width: int,
        height: int,
        format: str = 'rgba8unorm',
        usage: Optional[int] = None,
        label: Optional[str] = None
    ) -> 'GPUTexture':
        """
        Create GPU texture.

        Args:
            context: GPU context
            width: Texture width in pixels
            height: Texture height in pixels
            format: Texture format (default: 'rgba8unorm')
            usage: Texture usage flags (default: TEXTURE_BINDING | COPY_DST | COPY_SRC)
            label: Optional debug label

        Returns:
            GPUTexture instance

        Example:
            texture = GPUTexture.create(gpu_ctx, width=1920, height=1080)
        """
        if not HAS_WGPU:
            raise RuntimeError("WebGPU not available")

        texture = context.create_texture(
            width=width,
            height=height,
            format=format,
            usage=usage,
            label=label
        )

        return cls(
            context=context,
            texture=texture,
            width=width,
            height=height,
            format=format
        )

    def write(self, data: np.ndarray) -> None:
        """
        Upload numpy array to GPU texture.

        Args:
            data: NumPy array (H, W, 3) or (H, W, 4), dtype uint8

        Example:
            frame = np.random.randint(0, 255, (1080, 1920, 3), dtype=np.uint8)
            texture.write(frame)
        """
        if not isinstance(data, np.ndarray):
            raise TypeError(f"Expected numpy array, got {type(data)}")

        if data.dtype != np.uint8:
            raise TypeError(f"Expected dtype uint8, got {data.dtype}")

        height, width, channels = data.shape

        if width != self.width or height != self.height:
            raise ValueError(
                f"Data shape mismatch: expected ({self.height}, {self.width}, _), "
                f"got {data.shape}"
            )

        # Convert RGB to RGBA if needed
        if channels == 3:
            rgba = np.zeros((height, width, 4), dtype=np.uint8)
            rgba[:, :, :3] = data
            rgba[:, :, 3] = 255  # Fully opaque
            data = rgba
        elif channels != 4:
            raise ValueError(f"Expected 3 or 4 channels, got {channels}")

        # Ensure contiguous
        if not data.flags['C_CONTIGUOUS']:
            data = np.ascontiguousarray(data)

        # Upload to GPU texture
        self.context.queue.write_texture(
            {
                "texture": self.texture,
                "mip_level": 0,
                "origin": (0, 0, 0),
            },
            data,
            {
                "offset": 0,
                "bytes_per_row": width * 4,  # RGBA
                "rows_per_image": height,
            },
            (width, height, 1)
        )

    async def read(self, channels: int = 3) -> np.ndarray:
        """
        Download GPU texture to numpy array (async).

        Args:
            channels: Number of output channels (3 for RGB, 4 for RGBA)

        Returns:
            NumPy array (H, W, channels), dtype uint8

        Example:
            result = await texture.read()
        """
        # Create staging buffer for readback
        bytes_per_row = self.width * 4  # RGBA
        buffer_size = bytes_per_row * self.height

        staging_buffer = self.context.create_buffer(
            size=buffer_size,
            usage=wgpu.BufferUsage.COPY_DST | wgpu.BufferUsage.MAP_READ
        )

        # Copy texture → staging buffer
        encoder = self.context.device.create_command_encoder()
        encoder.copy_texture_to_buffer(
            {
                "texture": self.texture,
                "mip_level": 0,
                "origin": (0, 0, 0),
            },
            {
                "buffer": staging_buffer,
                "offset": 0,
                "bytes_per_row": bytes_per_row,
                "rows_per_image": self.height,
            },
            (self.width, self.height, 1)
        )
        self.context.queue.submit([encoder.finish()])

        # Map staging buffer to CPU
        await staging_buffer.map_async(wgpu.MapMode.READ)

        # Read data
        data_view = staging_buffer.get_mapped_range()
        data_rgba = np.frombuffer(data_view, dtype=np.uint8).copy()

        # Unmap
        staging_buffer.unmap()

        # Reshape to (H, W, 4)
        data_rgba = data_rgba.reshape((self.height, self.width, 4))

        # Return requested channels
        if channels == 3:
            return data_rgba[:, :, :3].copy()
        else:
            return data_rgba.copy()

    def __repr__(self) -> str:
        return f"GPUTexture(width={self.width}, height={self.height}, format={self.format})"
