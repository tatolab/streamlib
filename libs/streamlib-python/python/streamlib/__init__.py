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
        # setup() / teardown() — ctx is RuntimeContextFullAccess
        _, buffer = ctx.gpu_full_access.acquire_surface(1920, 1080, PixelFormat.BGRA32)
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


# Unified logging API — `streamlib.log.info(...)`, etc.
# Routes through the escalate-IPC `{op:"log"}` path to the host JSONL.
from . import log

# Canonical monotonic timestamp source — `clock_gettime(CLOCK_MONOTONIC)`.
# Use for any timestamp that crosses the host/subprocess boundary or is
# compared against another runtime's stamps. `MonotonicTimer` is the
# drift-free periodic timer (timerfd) for continuous-mode dispatch.
from . import clock
from .clock import MonotonicTimer, monotonic_now_ns

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

# Re-export capability-typed runtime context views for processor authors
from .processor_context import (
    NativeGpuContextFullAccess,
    NativeGpuContextLimitedAccess,
    NativeRuntimeContextFullAccess,
    NativeRuntimeContextLimitedAccess,
)

# Public type aliases for processor lifecycle annotations
RuntimeContextFullAccess = NativeRuntimeContextFullAccess
RuntimeContextLimitedAccess = NativeRuntimeContextLimitedAccess
GpuContextFullAccess = NativeGpuContextFullAccess
GpuContextLimitedAccess = NativeGpuContextLimitedAccess

__all__ = [
    # Unified logging
    "log",
    # Canonical timestamp source + drift-free periodic timer
    "clock",
    "monotonic_now_ns",
    "MonotonicTimer",
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
    # Capability-typed runtime context
    "RuntimeContextFullAccess",
    "RuntimeContextLimitedAccess",
    "GpuContextFullAccess",
    "GpuContextLimitedAccess",
    "NativeRuntimeContextFullAccess",
    "NativeRuntimeContextLimitedAccess",
    "NativeGpuContextFullAccess",
    "NativeGpuContextLimitedAccess",
]
