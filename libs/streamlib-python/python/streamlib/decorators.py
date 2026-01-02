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

    The decorated class should define input/output ports using @input
    and @output decorators on methods, and implement process() and
    optionally setup() and teardown().

    Args:
        name: Processor name for registry. Defaults to class name.
        description: Human-readable description.
        execution: Execution mode - "Reactive" or "Continuous".

    Example:
        @processor(name="GrayscaleProcessor", description="Convert to grayscale")
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
                    output = ctx.gpu.dispatch(self.shader, {"input_texture": texture},
                                              frame["width"], frame["height"])
                    ctx.output("video_out").set({"texture": output, **frame})
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


def input(
    name: Optional[str] = None,
    schema: Optional[str] = None,
    description: str = "",
):
    """Mark a method as defining an input port.

    Args:
        name: Port name. Defaults to method name.
        schema: Schema name from SCHEMA_REGISTRY (required). Examples: "VideoFrame", "AudioFrame", "DataFrame".
        description: Human-readable description for introspection.

    Example:
        @input(schema="VideoFrame", description="RGB video input")
        def video_in(self): pass
    """

    def decorator(method):
        port_name = name or method.__name__
        method._streamlib_input_port = {
            "name": port_name,
            "schema": schema,  # Required - error logged if None when get() called
            "description": description,
        }
        return method

    return decorator


def output(
    name: Optional[str] = None,
    schema: Optional[str] = None,
    description: str = "",
):
    """Mark a method as defining an output port.

    Args:
        name: Port name. Defaults to method name.
        schema: Schema name from SCHEMA_REGISTRY (required). Examples: "VideoFrame", "AudioFrame", "DataFrame".
        description: Human-readable description for introspection.

    Example:
        @output(schema="VideoFrame", description="Processed video output")
        def video_out(self): pass
    """

    def decorator(method):
        port_name = name or method.__name__
        method._streamlib_output_port = {
            "name": port_name,
            "schema": schema,  # Required - error logged if None when set() called
            "description": description,
        }
        return method

    return decorator


# Backward compatibility aliases
def input_port(
    name: Optional[str] = None,
    frame_type: str = "VideoFrame",
    description: str = "",
):
    """Deprecated: Use @input(schema=...) instead."""
    return input(name=name, schema=frame_type, description=description)


def output_port(
    name: Optional[str] = None,
    frame_type: str = "VideoFrame",
    description: str = "",
):
    """Deprecated: Use @output(schema=...) instead."""
    return output(name=name, schema=frame_type, description=description)
