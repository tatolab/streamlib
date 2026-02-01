# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""StreamLib Python subprocess SDK for real-time audio/video processing.

This package provides the Python subprocess bridge for StreamLib, allowing
Python processors to run as isolated subprocesses communicating with the
Rust runtime via a length-prefixed JSON protocol over stdin/stdout pipes.
"""

# Pixel format constants
class PixelFormat:
    """Pixel format constants for acquire_pixel_buffer().

    Usage:
        from streamlib import PixelFormat
        buffer = ctx.gpu.acquire_pixel_buffer(1920, 1080, PixelFormat.BGRA32)
    """
    BGRA32 = "bgra32"
    RGBA32 = "rgba32"
    ARGB32 = "argb32"
    RGBA64 = "rgba64"
    NV12_VIDEO = "nv12_video"
    NV12_FULL = "nv12_full"
    UYVY422 = "uyvy422"
    YUYV422 = "yuyv422"
    GRAY8 = "gray8"


# Re-export decorators and schema API
from .decorators import (
    # Processor decorators
    processor,
    input,
    output,
    # Schema decorator
    schema,
    # Field descriptors
    SchemaField,
    f32,
    f64,
    i32,
    i64,
    u32,
    u64,
    bool_field,
    # Deprecated aliases
    input_port,
    output_port,
)

__all__ = [
    # Processor decorators
    "processor",
    "input",
    "output",
    # Schema API
    "schema",
    "SchemaField",
    "f32",
    "f64",
    "i32",
    "i64",
    "u32",
    "u64",
    "bool_field",
    # Deprecated aliases
    "input_port",
    "output_port",
    "PixelFormat",
]
