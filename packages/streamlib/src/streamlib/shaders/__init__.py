"""
WGSL Shader Library for streamlib.

Provides pre-written WebGPU shaders for common video effects and operations.
These shaders are designed to work with streamlib's GPU-first architecture.

Example usage:
    from streamlib.shaders import BLUR_SHADER, EDGE_DETECT_SHADER

    # Use in a handler
    pipeline = gpu_ctx.create_compute_pipeline(BLUR_SHADER)
    gpu_ctx.run_compute(pipeline, input=frame.data, output=output_texture)
"""

from .blur import BLUR_SHADER, BOX_BLUR_SHADER, GAUSSIAN_BLUR_SHADER
from .filters import (
    GRAYSCALE_SHADER,
    SEPIA_SHADER,
    BRIGHTNESS_SHADER,
    CONTRAST_SHADER,
    SATURATION_SHADER,
    HUE_SHIFT_SHADER
)
from .effects import (
    EDGE_DETECT_SHADER,
    SHARPEN_SHADER,
    EMBOSS_SHADER,
    PIXELATE_SHADER,
    VIGNETTE_SHADER,
    CHROMATIC_ABERRATION_SHADER
)
from .transforms import (
    FLIP_HORIZONTAL_SHADER,
    FLIP_VERTICAL_SHADER,
    ROTATE_90_SHADER,
    SCALE_SHADER
)
from .blend import (
    ALPHA_BLEND_SHADER,
    ADDITIVE_BLEND_SHADER,
    MULTIPLY_BLEND_SHADER,
    SCREEN_BLEND_SHADER
)

__all__ = [
    # Blur shaders
    'BLUR_SHADER',
    'BOX_BLUR_SHADER',
    'GAUSSIAN_BLUR_SHADER',

    # Filter shaders
    'GRAYSCALE_SHADER',
    'SEPIA_SHADER',
    'BRIGHTNESS_SHADER',
    'CONTRAST_SHADER',
    'SATURATION_SHADER',
    'HUE_SHIFT_SHADER',

    # Effect shaders
    'EDGE_DETECT_SHADER',
    'SHARPEN_SHADER',
    'EMBOSS_SHADER',
    'PIXELATE_SHADER',
    'VIGNETTE_SHADER',
    'CHROMATIC_ABERRATION_SHADER',

    # Transform shaders
    'FLIP_HORIZONTAL_SHADER',
    'FLIP_VERTICAL_SHADER',
    'ROTATE_90_SHADER',
    'SCALE_SHADER',

    # Blend shaders
    'ALPHA_BLEND_SHADER',
    'ADDITIVE_BLEND_SHADER',
    'MULTIPLY_BLEND_SHADER',
    'SCREEN_BLEND_SHADER',
]