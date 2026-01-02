# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""StreamLib Python bindings for real-time audio/video processing.

This package provides Python bindings to StreamLib, allowing Python
processors to run within the Rust runtime. Python acts as a scripting
layer that orchestrates GPU shaders - heavy processing stays on GPU.

Example:
    from streamlib import processor, input, output

    @processor(name="GrayscaleProcessor")
    class GrayscaleProcessor:
        @input(schema="VideoFrame")
        def video_in(self): pass

        @output(schema="VideoFrame")
        def video_out(self): pass

        def setup(self, ctx):
            self.shader = ctx.gpu.compile_shader("grayscale", WGSL_CODE)

        def process(self, ctx):
            frame = ctx.input("video_in").get()
            if frame:
                texture = ctx.input("video_in").get("texture")
                output_tex = ctx.gpu.dispatch(
                    self.shader,
                    {"input_texture": texture},
                    frame["width"],
                    frame["height"]
                )
                ctx.output("video_out").set({"texture": output_tex, **frame})
"""

# Re-export decorators
from .decorators import processor, input, output, input_port, output_port

# Try to import native bindings (available when built with maturin)
try:
    from ._native import (
        VideoFrame,
        GpuContext,
        ProcessorContext,
        GpuTexture,
        CompiledShader,
    )
except ImportError:
    # Native bindings not available - decorators still work for metadata
    pass

__all__ = [
    # Decorators (always available)
    "processor",
    "input",
    "output",
    # Deprecated aliases
    "input_port",
    "output_port",
    # Native types (available when built)
    "VideoFrame",
    "GpuContext",
    "ProcessorContext",
    "GpuTexture",
    "CompiledShader",
]
