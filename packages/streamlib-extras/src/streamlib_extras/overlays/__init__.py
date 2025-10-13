"""Overlays: Text overlays, lower thirds, graphics, etc."""

from .lower_thirds import LowerThirdsHandler

try:
    from .lower_thirds_gpu import LowerThirdsGPUHandler
    _HAS_GPU_LOWER_THIRDS = True
except ImportError:
    _HAS_GPU_LOWER_THIRDS = False

try:
    from .text_overlay_gpu import GPUTextOverlayHandler
    _HAS_GPU_TEXT_OVERLAY = True
except ImportError:
    _HAS_GPU_TEXT_OVERLAY = False

__all__ = ['LowerThirdsHandler']

if _HAS_GPU_LOWER_THIRDS:
    __all__.append('LowerThirdsGPUHandler')

if _HAS_GPU_TEXT_OVERLAY:
    __all__.append('GPUTextOverlayHandler')
