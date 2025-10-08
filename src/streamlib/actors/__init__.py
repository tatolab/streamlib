"""
Actor implementations.

This package contains concrete actor implementations organized by type:
- video.py: Video actors (generators, processors, display)
- audio.py: Audio actors (generators, processors, output)
- io.py: File I/O actors (read/write)
- compositor.py: Compositor actor
- drawing.py: Drawing actor
- network.py: Network actors (Phase 4)
"""

from .video import TestPatternActor, DisplayActor
from .compositor import CompositorActor

__all__ = [
    'TestPatternActor',
    'DisplayActor',
    'CompositorActor',
]
