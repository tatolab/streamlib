"""Effects: Video processing effects (blur, compositing, etc.)"""

from .blur import BlurFilter
from .compositor import CompositorHandler

try:
    from .blur_gpu import BlurFilterGPU
    _HAS_GPU_BLUR = True
except ImportError:
    _HAS_GPU_BLUR = False

try:
    from .blur_metal import BlurFilterMetal
    _HAS_METAL_BLUR = True
except (ImportError, RuntimeError):
    _HAS_METAL_BLUR = False

try:
    from .compositor_multi import MultiInputCompositor
    _HAS_MULTI_COMPOSITOR = True
except ImportError:
    _HAS_MULTI_COMPOSITOR = False

__all__ = ['BlurFilter', 'CompositorHandler']

if _HAS_GPU_BLUR:
    __all__.append('BlurFilterGPU')

if _HAS_METAL_BLUR:
    __all__.append('BlurFilterMetal')

if _HAS_MULTI_COMPOSITOR:
    __all__.append('MultiInputCompositor')
