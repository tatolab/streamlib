"""
Stream handlers for video processing.

Handlers implement StreamHandler base class and can be composed into pipelines.
"""

from .test_pattern import TestPatternHandler
from .display import DisplayHandler

__all__ = [
    'TestPatternHandler',
    'DisplayHandler',
]
