"""
Sink implementations for video output.
"""

from .file_sink import FileSink
from .display_sink import DisplaySink
from .hls_sink import HLSSink

__all__ = ['FileSink', 'DisplaySink', 'HLSSink']
