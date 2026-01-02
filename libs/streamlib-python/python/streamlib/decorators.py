# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Decorators for defining StreamLib processors in Python.

These decorators mark Python classes as StreamLib processors and define
their input/output ports. The metadata is extracted by PythonHostProcessor
to integrate with the Rust runtime.
"""

from typing import Optional


def processor(
    name: Optional[str] = None,
    description: str = "",
    execution: str = "Reactive",
):
    """Mark a class as a StreamLib processor.

    The decorated class should define input/output ports using @input_port
    and @output_port decorators on methods, and implement process() and
    optionally setup() and teardown().

    Args:
        name: Processor name for registry. Defaults to class name.
        description: Human-readable description.
        execution: Execution mode - "Reactive" or "Continuous".

    Example:
        @processor(name="GrayscaleProcessor", description="Convert to grayscale")
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
                    output = ctx.gpu.dispatch(self.shader, {"input_texture": frame.texture},
                                              frame.width, frame.height)
                    ctx.outputs.video_out.write(frame.with_texture(output))
    """

    def decorator(cls):
        # Collect port metadata from decorated methods
        inputs = []
        outputs = []

        for attr_name in dir(cls):
            attr = getattr(cls, attr_name, None)
            if callable(attr):
                if hasattr(attr, "_streamlib_input_port"):
                    inputs.append(attr._streamlib_input_port)
                if hasattr(attr, "_streamlib_output_port"):
                    outputs.append(attr._streamlib_output_port)

        # Store metadata on class for extraction by PythonHostProcessor
        cls.__streamlib_metadata__ = {
            "name": name or cls.__name__,
            "description": description,
            "execution": execution,
            "inputs": inputs,
            "outputs": outputs,
        }

        return cls

    return decorator


def input_port(
    name: Optional[str] = None,
    frame_type: str = "VideoFrame",
    description: str = "",
):
    """Mark a method as defining an input port.

    Args:
        name: Port name. Defaults to method name.
        frame_type: Frame type - "VideoFrame", "AudioFrame", or "DataFrame".
        description: Human-readable description.

    Example:
        @input_port(frame_type="VideoFrame", description="Video input")
        def video_in(self): pass
    """

    def decorator(method):
        port_name = name or method.__name__
        method._streamlib_input_port = {
            "name": port_name,
            "frame_type": frame_type,
            "description": description,
        }
        return method

    return decorator


def output_port(
    name: Optional[str] = None,
    frame_type: str = "VideoFrame",
    description: str = "",
):
    """Mark a method as defining an output port.

    Args:
        name: Port name. Defaults to method name.
        frame_type: Frame type - "VideoFrame", "AudioFrame", or "DataFrame".
        description: Human-readable description.

    Example:
        @output_port(frame_type="VideoFrame", description="Video output")
        def video_out(self): pass
    """

    def decorator(method):
        port_name = name or method.__name__
        method._streamlib_output_port = {
            "name": port_name,
            "frame_type": frame_type,
            "description": description,
        }
        return method

    return decorator
