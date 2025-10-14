"""
GPU acceleration using WebGPU.

This module provides a unified GPU backend using WebGPU, which automatically
selects the best native backend per platform:
- macOS: Metal
- Windows: Direct3D 12
- Linux: Vulkan

Example:
    # Create GPU context
    gpu_ctx = await GPUContext.create()

    # Create buffer
    buffer = GPUBuffer.create(gpu_ctx, size=1920*1080*3)

    # Upload data
    import numpy as np
    frame = np.random.randint(0, 255, (1080, 1920, 3), dtype=np.uint8)
    buffer.write(frame)

    # Download data
    result = buffer.read()
"""

from .context import GPUContext
from .buffers import GPUBuffer, GPUTexture
from .compute import ComputeShader

__all__ = [
    'GPUContext',
    'GPUBuffer',
    'GPUTexture',
    'ComputeShader',
]
