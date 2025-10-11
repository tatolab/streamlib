"""
Metal GPU utilities for zero-copy operations on Apple Silicon.

Provides helpers for:
- Creating Metal devices and command queues
- Converting between numpy/torch and Metal textures
- Managing Metal texture lifecycle
"""

import numpy as np
from typing import Optional, Any, Tuple
import sys

# Check if we're on macOS before importing Metal
HAS_METAL = False
if sys.platform == 'darwin':
    try:
        import Metal
        import MetalPerformanceShaders as MPS
        HAS_METAL = True
    except ImportError:
        pass

# Optional torch support
try:
    import torch
    HAS_TORCH = True
except ImportError:
    HAS_TORCH = False


class MetalContext:
    """
    Singleton Metal context for managing Metal device and command queue.

    Usage:
        ctx = MetalContext.get()
        texture = ctx.create_texture(width=640, height=480)
    """

    _instance: Optional['MetalContext'] = None

    def __init__(self):
        """Initialize Metal device and command queue."""
        if not HAS_METAL:
            raise RuntimeError("Metal not available (are you on macOS?)")

        # Create Metal device (system default)
        self.device = Metal.MTLCreateSystemDefaultDevice()
        if self.device is None:
            raise RuntimeError("Failed to create Metal device")

        # Create command queue
        self.command_queue = self.device.newCommandQueue()
        if self.command_queue is None:
            raise RuntimeError("Failed to create Metal command queue")

    @classmethod
    def get(cls) -> 'MetalContext':
        """Get or create singleton Metal context."""
        if cls._instance is None:
            cls._instance = cls()
        return cls._instance

    def create_texture(
        self,
        width: int,
        height: int,
        pixel_format=None,
        shared=True
    ) -> Any:
        """
        Create a Metal texture.

        Args:
            width: Texture width in pixels
            height: Texture height in pixels
            pixel_format: Metal pixel format (default: RGBA8Unorm)
            shared: If True, use shared storage (CPU-accessible). If False, use private (GPU-only, faster)

        Returns:
            Metal texture object
        """
        if pixel_format is None:
            pixel_format = Metal.MTLPixelFormatRGBA8Unorm

        # Create texture descriptor
        desc = Metal.MTLTextureDescriptor.texture2DDescriptorWithPixelFormat_width_height_mipmapped_(
            pixel_format, width, height, False
        )
        desc.setUsage_(
            Metal.MTLTextureUsageShaderRead |
            Metal.MTLTextureUsageShaderWrite |
            Metal.MTLTextureUsageRenderTarget
        )

        # Shared mode allows CPU access (needed for upload/download)
        # Private mode is GPU-only (faster, but no CPU access)
        storage_mode = Metal.MTLStorageModeShared if shared else Metal.MTLStorageModePrivate
        desc.setStorageMode_(storage_mode)

        # Create texture
        texture = self.device.newTextureWithDescriptor_(desc)
        if texture is None:
            raise RuntimeError(f"Failed to create Metal texture ({width}x{height})")

        return texture

    def numpy_to_texture(self, array: np.ndarray) -> Any:
        """
        Convert numpy array to Metal texture.

        Args:
            array: NumPy array [H, W, 3] or [H, W, 4], dtype uint8, RGB(A)

        Returns:
            Metal texture with data uploaded to GPU
        """
        if array.ndim != 3 or array.shape[2] not in [3, 4]:
            raise ValueError(f"Expected array shape (H, W, 3) or (H, W, 4), got {array.shape}")

        if array.dtype != np.uint8:
            raise ValueError(f"Expected dtype uint8, got {array.dtype}")

        height, width, channels = array.shape

        # Convert RGB to RGBA if needed (Metal prefers RGBA)
        if channels == 3:
            rgba = np.zeros((height, width, 4), dtype=np.uint8)
            rgba[:, :, :3] = array
            rgba[:, :, 3] = 255  # Fully opaque
            array = rgba

        # Create texture
        texture = self.create_texture(width, height)

        # Upload data to texture
        region = Metal.MTLRegion((0, 0, 0), (width, height, 1))
        bytes_per_row = width * 4  # RGBA = 4 bytes per pixel

        # Copy data to GPU
        texture.replaceRegion_mipmapLevel_withBytes_bytesPerRow_(
            region,
            0,  # mipmap level
            array.tobytes(),
            bytes_per_row
        )

        return texture

    def texture_to_numpy(self, texture: Any, channels: int = 3) -> np.ndarray:
        """
        Convert Metal texture to numpy array.

        Args:
            texture: Metal texture
            channels: Number of output channels (3 for RGB, 4 for RGBA)

        Returns:
            NumPy array [H, W, channels], dtype uint8
        """
        width = texture.width()
        height = texture.height()

        # Create buffer to read texture data
        bytes_per_row = width * 4  # RGBA
        buffer_size = bytes_per_row * height

        # Allocate buffer
        rgba_data = bytearray(buffer_size)

        # Read texture data from GPU
        region = Metal.MTLRegion((0, 0, 0), (width, height, 1))
        texture.getBytes_bytesPerRow_fromRegion_mipmapLevel_(
            rgba_data,
            bytes_per_row,
            region,
            0  # mipmap level
        )

        # Convert to numpy array
        array_rgba = np.frombuffer(rgba_data, dtype=np.uint8).reshape((height, width, 4))

        # Return requested channels
        if channels == 3:
            return array_rgba[:, :, :3].copy()
        else:
            return array_rgba.copy()

    def torch_to_texture(self, tensor: Any) -> Any:
        """
        Convert PyTorch tensor (on MPS) to Metal texture.

        Args:
            tensor: PyTorch tensor [H, W, 3], dtype uint8, on MPS device

        Returns:
            Metal texture (zero-copy if possible)
        """
        if not HAS_TORCH:
            raise RuntimeError("PyTorch not available")

        # For now, copy through CPU (TODO: implement zero-copy via MPS Metal bridge)
        array = tensor.cpu().numpy()
        return self.numpy_to_texture(array)

    def texture_to_torch(self, texture: Any, device='mps') -> Any:
        """
        Convert Metal texture to PyTorch tensor.

        Args:
            texture: Metal texture
            device: Target device ('mps', 'cpu', 'cuda')

        Returns:
            PyTorch tensor [H, W, 3], dtype uint8
        """
        if not HAS_TORCH:
            raise RuntimeError("PyTorch not available")

        # For now, copy through CPU (TODO: implement zero-copy via MPS Metal bridge)
        array = self.texture_to_numpy(texture, channels=3)
        tensor = torch.from_numpy(array)

        if device != 'cpu':
            tensor = tensor.to(device)

        return tensor


    def apply_gaussian_blur(
        self,
        input_texture: Any,
        output_texture: Any,
        sigma: float
    ) -> None:
        """
        Apply Gaussian blur using Metal Performance Shaders.

        Args:
            input_texture: Input Metal texture
            output_texture: Output Metal texture (must be same size as input)
            sigma: Blur sigma (standard deviation)
        """
        if not HAS_METAL:
            raise RuntimeError("Metal not available")

        # Create MPS Gaussian blur filter
        blur = MPS.MPSImageGaussianBlur.alloc().initWithDevice_sigma_(
            self.device, sigma
        )

        # Create command buffer
        command_buffer = self.command_queue.commandBuffer()

        # Encode blur operation
        blur.encodeToCommandBuffer_sourceTexture_destinationTexture_(
            command_buffer,
            input_texture,
            output_texture
        )

        # Commit and wait for completion
        command_buffer.commit()
        command_buffer.waitUntilCompleted()


def check_metal_available() -> Tuple[bool, Optional[str]]:
    """
    Check if Metal is available on this system.

    Returns:
        (available, error_message) tuple
    """
    if sys.platform != 'darwin':
        return False, "Metal only available on macOS"

    if not HAS_METAL:
        return False, "Metal frameworks not installed (pip install pyobjc-framework-Metal pyobjc-framework-MetalPerformanceShaders)"

    try:
        ctx = MetalContext.get()
        return True, None
    except Exception as e:
        return False, f"Failed to initialize Metal: {e}"
