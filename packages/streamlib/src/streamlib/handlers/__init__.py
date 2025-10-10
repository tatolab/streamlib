"""
Stream handlers for video processing.

Handlers implement StreamHandler base class and can be composed into pipelines.
"""

# Phase 3.2: Basic Handlers
from .test_pattern import TestPatternHandler
from .display import DisplayHandler

# Phase 3.3: Advanced Handlers
from .blur import BlurFilter
from .compositor import CompositorHandler
from .drawing import DrawingHandler, DrawingContext

# Phase 3.4: GPU Support
try:
    from .blur_gpu import BlurFilterGPU
    _HAS_GPU_BLUR = True
except ImportError:
    _HAS_GPU_BLUR = False

try:
    from .display_gpu import DisplayGPUHandler
    _HAS_GPU_DISPLAY = True
except ImportError:
    _HAS_GPU_DISPLAY = False

try:
    from .text_overlay_gpu import GPUTextOverlayHandler
    _HAS_GPU_TEXT_OVERLAY = True
except ImportError:
    _HAS_GPU_TEXT_OVERLAY = False

__all__ = [
    # Phase 3.2
    'TestPatternHandler',
    'DisplayHandler',

    # Phase 3.3
    'BlurFilter',
    'CompositorHandler',
    'DrawingHandler',
    'DrawingContext',
]

# Phase 3.4 (conditional)
if _HAS_GPU_BLUR:
    __all__.append('BlurFilterGPU')

if _HAS_GPU_DISPLAY:
    __all__.append('DisplayGPUHandler')

if _HAS_GPU_TEXT_OVERLAY:
    __all__.append('GPUTextOverlayHandler')

# Export Metal blur handler (macOS only, conditional)
try:
    from .blur_metal import BlurFilterMetal
    __all__.append('BlurFilterMetal')
except (ImportError, RuntimeError):
    # Metal not available (not macOS or missing dependencies)
    pass

