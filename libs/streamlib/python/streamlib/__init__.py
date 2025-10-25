"""streamlib - Real-time streaming infrastructure for AI agents"""

from .streamlib import *

__doc__ = streamlib.__doc__
if hasattr(streamlib, "__all__"):
    __all__ = streamlib.__all__

# Marker classes for @processor decorator type hints
# These allow syntax like: video = StreamInput(VideoFrame)
# The actual ports are created in Rust and injected at runtime

class StreamInput:
    """Marker class for input port type hints in @processor decorator"""
    def __init__(self, type_hint=None):
        pass
    def __repr__(self):
        return "StreamInput(VideoFrame)"

class StreamOutput:
    """Marker class for output port type hints in @processor decorator"""
    def __init__(self, type_hint=None):
        pass
    def __repr__(self):
        return "StreamOutput(VideoFrame)"
