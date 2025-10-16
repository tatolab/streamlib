"""
GPU acceleration using WebGPU.

This module provides a unified GPU backend using WebGPU, which automatically
selects the best native backend per platform:
- macOS: Metal
- Windows: Direct3D 12
- Linux: Vulkan

Example:
    gpu_ctx = await GPUContext.create()
    texture = gpu_ctx.create_texture(1920, 1080)
    pipeline = gpu_ctx.create_compute_pipeline(shader_code)
    gpu_ctx.run_compute(pipeline, input=texture, output=output_texture)
"""

from .context import GPUContext
from .compute import ComputeShader
from .renderer import GPURenderer
from .ml import MLRuntime, ONNXModel

__all__ = [
    'GPUContext',
    'ComputeShader',
    'GPURenderer',
    'MLRuntime',
    'ONNXModel',
]
