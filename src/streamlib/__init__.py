"""
streamlib - A composable streaming library for Python

This library provides Unix-pipe-style composable primitives for video streaming:
- Sources: Webcam, files, screen capture, network streams, generated content
- Sinks: HLS, files, display, network
- Layers: Video, drawing (Skia), ML models
- Compositor: Zero-copy numpy pipeline for layer composition

The library is designed to be:
- Network-transparent: Operations work locally or remotely
- Distributed: Chain operations across machines
- Mesh-capable: Multiple machines collaborate on processing
- Zero-dependency: No GStreamer or system packages required (uses PyAV)
"""

# Core base classes
from .base import (
    StreamSource,
    StreamSink,
    Layer,
    Compositor,
    TimestampedFrame,
)

# Timing infrastructure
from .timing import (
    TimedTick,
    Clock,
    SoftwareClock,
    FrameTimer,
    PTPClient,
    MultiStreamSynchronizer,
    SyncedFrame,
    estimate_fps,
    align_timestamps,
)

# Stream orchestration
from .stream import (
    Stream,
)

# Plugin system
from .plugins import (
    PluginRegistry,
    get_registry,
    register_source,
    register_sink,
    register_layer,
    register_compositor,
)

# Drawing layers
from .drawing import (
    DrawingContext,
    DrawingLayer,
    VideoLayer,
)

# Compositor
from .compositor import (
    DefaultCompositor,
)

# Sources
from .sources import (
    FileSource,
    TestSource,
)

# Sinks
from .sinks import (
    FileSink,
    DisplaySink,
    HLSSink,
)

__version__ = "0.1.0"

__all__ = [
    # Base classes
    "StreamSource",
    "StreamSink",
    "Layer",
    "Compositor",
    "TimestampedFrame",
    # Stream orchestration
    "Stream",
    # Timing
    "TimedTick",
    "Clock",
    "SoftwareClock",
    "FrameTimer",
    "PTPClient",
    "MultiStreamSynchronizer",
    "SyncedFrame",
    "estimate_fps",
    "align_timestamps",
    # Plugins
    "PluginRegistry",
    "get_registry",
    "register_source",
    "register_sink",
    "register_layer",
    "register_compositor",
    # Drawing
    "DrawingContext",
    "DrawingLayer",
    "VideoLayer",
    # Compositor
    "DefaultCompositor",
    # Sources
    "FileSource",
    "TestSource",
    # Sinks
    "FileSink",
    "DisplaySink",
    "HLSSink",
]
