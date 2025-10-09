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
