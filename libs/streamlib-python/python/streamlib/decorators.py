# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Decorators for defining StreamLib processors and schemas in Python.

These decorators mark Python classes as StreamLib processors and define
their input/output ports. The metadata is extracted by PythonHostProcessor
to integrate with the Rust runtime.

Schema decorators allow defining custom data schemas that are backed by
Rust's DynamicDataFrameSchema, enabling seamless data flow between Python
and Rust processors.
"""

from typing import Optional, Union, List, Type


# =============================================================================
# Schema Field Descriptors
# =============================================================================


class SchemaField:
    """Descriptor for a field in a schema.

    Used internally by field descriptor functions (f32, i64, etc.) to define
    schema fields. The @schema decorator collects these to build the schema.
    """

    def __init__(
        self,
        primitive_type: str,
        shape: Optional[List[int]] = None,
        description: str = "",
    ):
        self.primitive_type = primitive_type
        self.shape = shape or []
        self.description = description

    def __repr__(self) -> str:
        if self.shape:
            return f"SchemaField({self.primitive_type}, shape={self.shape})"
        return f"SchemaField({self.primitive_type})"


def f32(shape: Optional[List[int]] = None, description: str = "") -> SchemaField:
    """Define a 32-bit float field.

    Args:
        shape: Array dimensions. Empty/None for scalar, [512] for 1D array, [4, 4] for 2D.
        description: Human-readable description.

    Example:
        @schema(name="embedding")
        class EmbeddingSchema:
            vector = f32(shape=[512], description="Feature vector")
            confidence = f32(description="Confidence score")
    """
    return SchemaField("f32", shape, description)


def f64(shape: Optional[List[int]] = None, description: str = "") -> SchemaField:
    """Define a 64-bit float field."""
    return SchemaField("f64", shape, description)


def i32(shape: Optional[List[int]] = None, description: str = "") -> SchemaField:
    """Define a 32-bit signed integer field."""
    return SchemaField("i32", shape, description)


def i64(shape: Optional[List[int]] = None, description: str = "") -> SchemaField:
    """Define a 64-bit signed integer field."""
    return SchemaField("i64", shape, description)


def u32(shape: Optional[List[int]] = None, description: str = "") -> SchemaField:
    """Define a 32-bit unsigned integer field."""
    return SchemaField("u32", shape, description)


def u64(shape: Optional[List[int]] = None, description: str = "") -> SchemaField:
    """Define a 64-bit unsigned integer field."""
    return SchemaField("u64", shape, description)


def bool_field(description: str = "") -> SchemaField:
    """Define a boolean field.

    Note: Named bool_field to avoid shadowing Python's bool builtin.
    """
    return SchemaField("bool", None, description)


# =============================================================================
# Schema Decorator
# =============================================================================


def schema(name: Optional[str] = None):
    """Define a custom data schema backed by Rust.

    The decorated class should have class attributes that are SchemaField
    instances (created via f32, i64, bool_field, etc.). The decorator
    collects these fields and creates a Rust-backed DynamicDataFrameSchema.

    Args:
        name: Schema name for registry. Defaults to class name.

    Example:
        @schema(name="clip_embedding")
        class ClipEmbeddingSchema:
            embedding = f32(shape=[512], description="CLIP embedding vector")
            timestamp = i64(description="Timestamp in nanoseconds")
            normalized = bool_field(description="Whether embedding is normalized")

        # Use in processor:
        @processor(name="EmbeddingProcessor")
        class EmbeddingProcessor:
            @output(schema=ClipEmbeddingSchema)
            def embedding_out(self): pass
    """

    def decorator(cls):
        schema_name = name or cls.__name__

        # Collect field definitions from class attributes
        fields = []
        for attr_name in dir(cls):
            if attr_name.startswith("_"):
                continue
            attr_value = getattr(cls, attr_name, None)
            if isinstance(attr_value, SchemaField):
                fields.append(
                    {
                        "name": attr_name,
                        "primitive_type": attr_value.primitive_type,
                        "shape": attr_value.shape,
                        "description": attr_value.description,
                    }
                )

        # Store metadata on class
        cls.__streamlib_schema__ = {
            "name": schema_name,
            "fields": fields,
        }

        return cls

    return decorator


def _get_schema_name(schema_arg: Union[str, Type, None]) -> Optional[str]:
    """Extract schema name from string or schema class."""
    if schema_arg is None:
        return None
    if isinstance(schema_arg, str):
        return schema_arg
    # Check if it's a schema-decorated class
    if hasattr(schema_arg, "__streamlib_schema__"):
        return schema_arg.__streamlib_schema__["name"]
    # Fallback to class name
    if isinstance(schema_arg, type):
        return schema_arg.__name__
    return None


# =============================================================================
# Processor Decorators
# =============================================================================


def processor(
    name: Optional[str] = None,
    description: str = "",
    execution: str = "Reactive",
):
    """Mark a class as a StreamLib processor.

    The decorated class should define input/output ports using @input
    and @output decorators on methods, and implement the appropriate
    methods for the execution mode.

    Args:
        name: Processor name for registry. Defaults to class name.
        description: Human-readable description.
        execution: Execution mode - one of:
            - "Reactive": process() called when input data arrives.
              Use for transforms, filters, effects, encoders, decoders.
            - "Continuous": process() called repeatedly in a loop.
              Use for generators, sources, polling, batch processing.
            - "Manual": start() called once, then you control timing.
              Use for hardware callbacks, display vsync, cameras.

    Required methods by execution mode:
        Reactive/Continuous:
            - process(self, ctx): Called to process data.

        Manual:
            - start(self, ctx): Called once to start the processor.
            - stop(self, ctx): Optional, called when stopping.

    Optional lifecycle methods (all modes):
        - setup(self, ctx): Called once at startup.
        - teardown(self, ctx): Called once at shutdown.
        - on_pause(self, ctx): Called when processor is paused.
        - on_resume(self, ctx): Called when processor is resumed.

    Example (Reactive):
        @processor(name="GrayscaleProcessor", execution="Reactive")
        class GrayscaleProcessor:
            @input(schema="VideoFrame")
            def video_in(self): pass

            @output(schema="VideoFrame")
            def video_out(self): pass

            def process(self, ctx):
                frame = ctx.input("video_in").get()
                if frame:
                    # Process frame...
                    ctx.output("video_out").set(result)

    Example (Manual):
        @processor(name="CameraSource", execution="Manual")
        class CameraSource:
            @output(schema="VideoFrame")
            def video_out(self): pass

            def start(self, ctx):
                # Start camera capture, register callbacks
                self.camera = Camera()
                self.camera.on_frame(lambda f: ctx.output("video_out").set(f))
                self.camera.start()

            def stop(self, ctx):
                self.camera.stop()
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
    schema: Union[str, Type, None] = None,
    description: str = "",
):
    """Mark a method as defining an input port.

    Args:
        name: Port name. Defaults to method name.
        schema: Schema name (str) or schema class decorated with @schema.
            Examples: "VideoFrame", "AudioFrame", or MyCustomSchema.
        description: Human-readable description for introspection.

    Example:
        @input(schema="VideoFrame", description="RGB video input")
        def video_in(self): pass

        @input(schema=MyCustomSchema)
        def custom_in(self): pass
    """

    def decorator(method):
        port_name = name or method.__name__
        method._streamlib_input_port = {
            "name": port_name,
            "schema": _get_schema_name(schema),
            "description": description,
        }
        return method

    return decorator


def output(
    name: Optional[str] = None,
    schema: Union[str, Type, None] = None,
    description: str = "",
):
    """Mark a method as defining an output port.

    Args:
        name: Port name. Defaults to method name.
        schema: Schema name (str) or schema class decorated with @schema.
            Examples: "VideoFrame", "AudioFrame", or MyCustomSchema.
        description: Human-readable description for introspection.

    Example:
        @output(schema="VideoFrame", description="Processed video output")
        def video_out(self): pass

        @output(schema=MyCustomSchema)
        def custom_out(self): pass
    """

    def decorator(method):
        port_name = name or method.__name__
        method._streamlib_output_port = {
            "name": port_name,
            "schema": _get_schema_name(schema),
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
