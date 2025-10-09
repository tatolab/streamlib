"""
Reference actor implementations.

These are EXAMPLES, not part of the streamlib core package.
They demonstrate how to build actors using external libraries.

Use these as starting points for your own implementations.
"""

from .video import TestPatternActor, DisplayActor
from .compositor import CompositorActor
from .drawing import DrawingActor

__all__ = [
    'TestPatternActor',
    'DisplayActor',
    'CompositorActor',
    'DrawingActor',
]
