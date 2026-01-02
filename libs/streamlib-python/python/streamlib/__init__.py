# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""StreamLib Python bindings for real-time audio/video processing.

This package provides Python bindings to StreamLib, allowing Python
processors to run within the Rust runtime. Python acts as a scripting
layer that orchestrates GPU shaders - heavy processing stays on GPU.

Example:
    from streamlib import processor, input_port, output_port

    @processor(name="GrayscaleProcessor")
    class GrayscaleProcessor:
        @input_port(frame_type="VideoFrame")
        def video_in(self): pass

        @output_port(frame_type="VideoFrame")
        def video_out(self): pass

        def setup(self, ctx):
            self.shader = ctx.gpu.compile_shader("grayscale", WGSL_CODE)

        def process(self, ctx):
            frame = ctx.inputs.video_in.read()
            if frame:
                output = ctx.gpu.dispatch(
                    self.shader,
                    {"input_texture": frame.texture},
                    frame.width,
                    frame.height
                )
                ctx.outputs.video_out.write(frame.with_texture(output))
"""

# Re-export decorators
from .decorators import processor, input_port, output_port

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
    "input_port",
    "output_port",
    # Native types (available when built)
    "VideoFrame",
    "GpuContext",
    "ProcessorContext",
    "GpuTexture",
    "CompiledShader",
]
