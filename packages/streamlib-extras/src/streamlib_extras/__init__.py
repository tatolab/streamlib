"""
streamlib-extras: Reference handler implementations for streamlib.

This package provides ready-to-use handlers for common video streaming operations:
- Patterns: TestPatternHandler
- Camera: CameraHandler, CameraHandlerGPU
- Display: DisplayHandler, DisplayGPUHandler
- Effects: BlurFilter, BlurFilterGPU, BlurFilterMetal, CompositorHandler, MultiInputCompositor
- Overlays: LowerThirdsHandler, LowerThirdsGPUHandler, GPUTextOverlayHandler
- Utils: DrawingHandler, DrawingContext

Install: pip install streamlib-extras
"""

# Patterns
from .test_pattern import TestPatternHandler

# Camera handlers
from .camera import CameraHandler
try:
    from .camera_gpu import CameraHandlerGPU
    _HAS_GPU_CAMERA = True
except ImportError:
    _HAS_GPU_CAMERA = False

# Display handlers
from .display import DisplayHandler
try:
    from .display_gpu import DisplayGPUHandler
    _HAS_GPU_DISPLAY = True
except ImportError:
    _HAS_GPU_DISPLAY = False

# Drawing utilities
from .drawing import DrawingHandler, DrawingContext

# Effects
from .effects.blur import BlurFilter
try:
    from .effects.blur_gpu import BlurFilterGPU
    _HAS_GPU_BLUR = True
except ImportError:
    _HAS_GPU_BLUR = False

try:
    from .effects.blur_metal import BlurFilterMetal
    _HAS_METAL_BLUR = True
except (ImportError, RuntimeError):
    _HAS_METAL_BLUR = False

from .effects.compositor import CompositorHandler
try:
    from .effects.compositor_multi import MultiInputCompositor
    _HAS_MULTI_COMPOSITOR = True
except ImportError:
    _HAS_MULTI_COMPOSITOR = False

# Overlays
from .overlays.lower_thirds import LowerThirdsHandler
try:
    from .overlays.lower_thirds_gpu import LowerThirdsGPUHandler
    _HAS_GPU_LOWER_THIRDS = True
except ImportError:
    _HAS_GPU_LOWER_THIRDS = False

try:
    from .overlays.text_overlay_gpu import GPUTextOverlayHandler
    _HAS_GPU_TEXT_OVERLAY = True
except ImportError:
    _HAS_GPU_TEXT_OVERLAY = False


__all__ = [
    # Patterns
    'TestPatternHandler',

    # Camera
    'CameraHandler',

    # Display
    'DisplayHandler',

    # Drawing
    'DrawingHandler',
    'DrawingContext',

    # Effects
    'BlurFilter',
    'CompositorHandler',

    # Overlays
    'LowerThirdsHandler',
]

# Add conditional exports
if _HAS_GPU_CAMERA:
    __all__.append('CameraHandlerGPU')

if _HAS_GPU_DISPLAY:
    __all__.append('DisplayGPUHandler')

if _HAS_GPU_BLUR:
    __all__.append('BlurFilterGPU')

if _HAS_METAL_BLUR:
    __all__.append('BlurFilterMetal')

if _HAS_MULTI_COMPOSITOR:
    __all__.append('MultiInputCompositor')

if _HAS_GPU_LOWER_THIRDS:
    __all__.append('LowerThirdsGPUHandler')

if _HAS_GPU_TEXT_OVERLAY:
    __all__.append('GPUTextOverlayHandler')

__version__ = '0.1.0'
