# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""StreamLib Python bindings for real-time audio/video processing.

This package provides Python bindings to StreamLib, allowing Python
processors to run within the Rust runtime. Python acts as a scripting
layer that orchestrates GPU shaders - heavy processing stays on GPU.

Example:
    from streamlib import processor, input, output, schema, f32, i64, bool_field

    # Define a custom schema
    @schema(name="clip_embedding")
    class ClipEmbeddingSchema:
        embedding = f32(shape=[512], description="CLIP embedding vector")
        timestamp = i64(description="Timestamp in nanoseconds")
        normalized = bool_field()

    @processor(name="EmbeddingProcessor")
    class EmbeddingProcessor:
        @input(schema="VideoFrame")
        def video_in(self): pass

        @output(schema=ClipEmbeddingSchema)
        def embedding_out(self): pass

        def process(self, ctx):
            frame = ctx.input("video_in").get()
            if frame:
                ctx.output("embedding_out").set({
                    "embedding": self.model.encode(frame),
                    "timestamp": frame["timestamp_ns"],
                    "normalized": True,
                })
"""

# Pixel format constants (pure Python - avoids PyO3 type identity issues)
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

# Try to import native bindings (available when built with maturin)
try:
    from ._native import (
        VideoFrame,
        GpuContext,
        ProcessorContext,
        GpuTexture,
    )
except ImportError:
    # Native bindings not available - decorators still work for metadata
    pass

__all__ = [
    # Processor decorators (always available)
    "processor",
    "input",
    "output",
    # Schema API (always available)
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
    # Native types (available when built)
    "VideoFrame",
    "GpuContext",
    "ProcessorContext",
    "GpuTexture",
    "PixelFormat",
]
