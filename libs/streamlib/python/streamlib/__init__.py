"""streamlib - Real-time streaming infrastructure for AI agents"""

from .streamlib import *
import inspect
from typing import Any, get_type_hints, Optional

__doc__ = streamlib.__doc__
if hasattr(streamlib, "__all__"):
    __all__ = streamlib.__all__

# Marker classes for @processor decorator type hints
# These allow syntax like: video = StreamInput(VideoFrame)
# The actual ports are created in Rust and injected at runtime

class StreamInput:
    """Marker class for input port type hints in @processor decorator"""
    def __init__(self, type_hint=None):
        pass
    def __repr__(self):
        return "StreamInput(VideoFrame)"

class StreamOutput:
    """Marker class for output port type hints in @processor decorator"""
    def __init__(self, type_hint=None):
        pass
    def __repr__(self):
        return "StreamOutput(VideoFrame)"


# Type stubs for frame types (actual types defined in Rust)
class VideoFrame:
    """Video frame type marker for port declarations"""
    pass

class AudioFrame:
    """Audio frame type marker for port declarations"""
    def __init__(self, channels=2):
        self.channels = channels

class DataFrame:
    """Generic data frame type marker for port declarations"""
    pass


# ========== NEW FIELD-BASED DECORATORS (Phase 1) ==========

class InputDescriptor:
    """Descriptor for input port fields (created by @input decorator)"""
    def __init__(self, name: str, description: str, required: bool, frame_type: str):
        self.name = name
        self.description = description
        self.required = required
        self.frame_type = frame_type  # e.g. "VideoFrame", "AudioFrame<2>", "DataFrame"
        self._port = None

    def __get__(self, obj, objtype=None):
        if obj is None:
            return self
        return self._port

    def __set_name__(self, owner, name):
        self.name = name


class OutputDescriptor:
    """Descriptor for output port fields (created by @output decorator)"""
    def __init__(self, name: str, description: str, frame_type: str):
        self.name = name
        self.description = description
        self.frame_type = frame_type  # e.g. "VideoFrame", "AudioFrame<2>", "DataFrame"
        self._port = None

    def __get__(self, obj, objtype=None):
        if obj is None:
            return self
        return self._port

    def __set_name__(self, owner, name):
        self.name = name


class ConfigDescriptor:
    """Descriptor for config fields (created by @config decorator)"""
    def __init__(self, name: str, default: Any):
        self.name = name
        self.default = default

    def __get__(self, obj, objtype=None):
        if obj is None:
            return self
        return getattr(obj, f'_config_{self.name}', self.default)

    def __set__(self, obj, value):
        setattr(obj, f'_config_{self.name}', value)

    def __set_name__(self, owner, name):
        self.name = name


def input(description: str = "", required: bool = True, type_hint: Any = None):
    """
    Field marker to declare an input port (matches Rust #[input] attribute).

    Usage:
        @StreamProcessor(mode="Pull", description="Edge detector")
        class EdgeDetector:
            video_in = input(description="Video frames to process")

            def process(self):
                frame = self.video_in.read_latest()  # Direct field access!

    Args:
        description: Human-readable description of this input port
        required: Whether this port must be connected
        type_hint: Frame type (VideoFrame, AudioFrame, etc.)
    """
    # Return a descriptor that will be detected by @StreamProcessor
    frame_type = type_hint.__name__ if type_hint and hasattr(type_hint, '__name__') else 'VideoFrame'
    return InputDescriptor('', description, required, frame_type)


def output(description: str = "", type_hint: Any = None):
    """
    Field marker to declare an output port (matches Rust #[output] attribute).

    Usage:
        @StreamProcessor(mode="Pull", description="Edge detector")
        class EdgeDetector:
            video_out = output(description="Processed video frames")

            def process(self):
                self.video_out.write(processed_frame)  # Direct field access!

    Args:
        description: Human-readable description of this output port
        type_hint: Frame type (VideoFrame, AudioFrame, etc.)
    """
    # Return a descriptor that will be detected by @StreamProcessor
    frame_type = type_hint.__name__ if type_hint and hasattr(type_hint, '__name__') else 'VideoFrame'
    return OutputDescriptor('', description, frame_type)


def video_input(description: str = "", required: bool = True):
    """
    Field marker for video input port (VideoFrame).

    Usage:
        video_in = video_input(description="Video frames to process")
    """
    return InputDescriptor('', description, required, 'VideoFrame')


def video_output(description: str = ""):
    """
    Field marker for video output port (VideoFrame).

    Usage:
        video_out = video_output(description="Processed video frames")
    """
    return OutputDescriptor('', description, 'VideoFrame')


def audio_input(description: str = "", required: bool = True, channels: int = 1):
    """
    Field marker for audio input port (AudioFrame<CHANNELS>).

    Usage:
        audio_in = audio_input(description="Audio input", channels=2)

    Args:
        description: Human-readable description
        required: Whether this port must be connected
        channels: Number of audio channels (1=mono, 2=stereo, etc.)
    """
    return InputDescriptor('', description, required, f'AudioFrame<{channels}>')


def audio_output(description: str = "", channels: int = 1):
    """
    Field marker for audio output port (AudioFrame<CHANNELS>).

    Usage:
        audio_out = audio_output(description="Audio output", channels=2)

    Args:
        description: Human-readable description
        channels: Number of audio channels (1=mono, 2=stereo, etc.)
    """
    return OutputDescriptor('', description, f'AudioFrame<{channels}>')


def data_input(description: str = "", required: bool = True):
    """
    Field marker for data input port (DataFrame).

    Usage:
        data_in = data_input(description="Generic data input")
    """
    return InputDescriptor('', description, required, 'DataFrame')


def data_output(description: str = ""):
    """
    Field marker for data output port (DataFrame).

    Usage:
        data_out = data_output(description="Generic data output")
    """
    return OutputDescriptor('', description, 'DataFrame')


def config(default: Any = None):
    """
    Field marker to declare a configuration field (matches Rust #[config] attribute).

    Usage:
        @StreamProcessor(mode="Pull", description="Edge detector")
        class EdgeDetector:
            threshold = config(100)

            def on_start(self, ctx):
                print(f"Threshold: {self.threshold}")  # Access config value!

    Args:
        default: Default value for the config field
    """
    # Return a descriptor that will be detected by @StreamProcessor
    return ConfigDescriptor('', default)


# ========== HELPER FUNCTIONS FOR METADATA EXTRACTION ==========

def _extract_field_decorators(cls):
    """
    Extract input(), output(), config() descriptor fields from a class.

    Returns:
        tuple: (inputs_dict, outputs_dict, config_fields_list)
    """
    inputs = {}
    outputs = {}
    config_fields = []

    # Scan class attributes for descriptor fields
    for attr_name in dir(cls):
        if attr_name.startswith('_'):
            continue

        try:
            attr = getattr(cls, attr_name)
        except AttributeError:
            continue

        # Check if this attribute is an InputDescriptor
        if isinstance(attr, InputDescriptor):
            inputs[attr_name] = {
                'name': attr_name,
                'type': attr.frame_type,
                'required': attr.required,
                'description': attr.description,
            }

        # Check if this attribute is an OutputDescriptor
        elif isinstance(attr, OutputDescriptor):
            outputs[attr_name] = {
                'name': attr_name,
                'type': attr.frame_type,
                'description': attr.description,
            }

        # Check if this attribute is a ConfigDescriptor
        elif isinstance(attr, ConfigDescriptor):
            config_fields.append(attr_name)

    return inputs, outputs, config_fields


def _extract_frame_type(type_hint):
    """
    Extract frame type from type hint like StreamInput[VideoFrame].

    Args:
        type_hint: Type annotation (e.g., StreamInput[VideoFrame])

    Returns:
        str: Frame type name (e.g., "VideoFrame")
    """
    # Check if it's a generic type like StreamInput[VideoFrame]
    if hasattr(type_hint, '__origin__') and hasattr(type_hint, '__args__'):
        if type_hint.__args__:
            frame_class = type_hint.__args__[0]
            return frame_class.__name__ if hasattr(frame_class, '__name__') else 'VideoFrame'

    # Fallback: check if it's StreamInput or StreamOutput directly
    if hasattr(type_hint, '__name__'):
        if type_hint.__name__ in ['StreamInput', 'StreamOutput']:
            return 'VideoFrame'  # Default
        return type_hint.__name__

    return 'VideoFrame'  # Default fallback


def _extract_nested_classes(cls):
    """
    Extract InputPorts and OutputPorts nested classes (backward compatibility).

    Returns:
        tuple: (inputs_dict, outputs_dict)
    """
    inputs = {}
    outputs = {}

    # Check for InputPorts nested class
    if hasattr(cls, 'InputPorts'):
        input_ports_class = cls.InputPorts
        for attr_name in dir(input_ports_class):
            if attr_name.startswith('_'):
                continue
            try:
                attr = getattr(input_ports_class, attr_name)
                # Check if it's a StreamInput marker
                if isinstance(attr, StreamInput):
                    inputs[attr_name] = {
                        'name': attr_name,
                        'type': 'VideoFrame',  # Default for now
                        'required': True,
                        'description': '',
                    }
            except AttributeError:
                continue

    # Check for OutputPorts nested class
    if hasattr(cls, 'OutputPorts'):
        output_ports_class = cls.OutputPorts
        for attr_name in dir(output_ports_class):
            if attr_name.startswith('_'):
                continue
            try:
                attr = getattr(output_ports_class, attr_name)
                # Check if it's a StreamOutput marker
                if isinstance(attr, StreamOutput):
                    outputs[attr_name] = {
                        'name': attr_name,
                        'type': 'VideoFrame',  # Default for now
                        'description': '',
                    }
            except AttributeError:
                continue

    return inputs, outputs


# ========== OLD METHOD-BASED DECORATORS (Deprecated, kept for compatibility) ==========

def input_port(name: str, type: type, required: bool = True):
    """
    [DEPRECATED] Method-based decorator to mark a method as an input port.
    Use @input field decorator instead.

    Usage:
        @input_port(name="video_in", type=VideoFrame)
        def video_in(self):
            return self._video_in

    Args:
        name: Port name (used for connections)
        type: Frame type (VideoFrame, AudioFrame, DataFrame)
        required: Whether this port must be connected
    """
    def decorator(func):
        # Store metadata on function for introspection
        func.__streamlib_input__ = {
            'name': name,
            'type': type.__name__ if hasattr(type, '__name__') else str(type),
            'required': required,
            'channels': getattr(type, 'channels', None) if type.__name__ == 'AudioFrame' else None,
        }
        return func
    return decorator


def output_port(name: str, type: type):
    """
    [DEPRECATED] Method-based decorator to mark a method as an output port.
    Use @output field decorator instead.

    Usage:
        @output_port(name="video_out", type=VideoFrame)
        def video_out(self):
            return self._video_out

    Args:
        name: Port name (used for connections)
        type: Frame type (VideoFrame, AudioFrame, DataFrame)
    """
    def decorator(func):
        # Store metadata on function for introspection
        func.__streamlib_output__ = {
            'name': name,
            'type': type.__name__ if hasattr(type, '__name__') else str(type),
            'channels': getattr(type, 'channels', None) if type.__name__ == 'AudioFrame' else None,
        }
        return func
    return decorator


# ========== HYBRID @StreamProcessor DECORATOR ==========

def StreamProcessor(cls_or_mode=None, *, mode: str = "Push", description: str = "", tags: list = None):
    """
    Hybrid class decorator that supports both field decorators and nested classes.

    NEW USAGE (Field Decorators - matches Rust macro):
        @StreamProcessor(mode="Pull", description="Edge detector")
        class EdgeDetector:
            @input(description="Video frames to process")
            video_in: StreamInput[VideoFrame]

            @output(description="Processed video frames")
            video_out: StreamOutput[VideoFrame]

            @config
            threshold: int = 100

            def on_start(self, ctx):
                self.gpu = ctx.gpu

            def process(self):
                frame = self.video_in.read_latest()  # Direct field access!
                self.video_out.write(processed)

    OLD USAGE (Nested Classes - backward compatible):
        @StreamProcessor
        class EdgeDetector:
            class InputPorts:
                video = StreamInput(VideoFrame)

            class OutputPorts:
                video = StreamOutput(VideoFrame)

            def process(self, tick):
                frame = self.input_ports().video.read_latest()
                self.output_ports().video.write(processed)

    Args:
        mode: Processing mode ("Push" or "Pull")
        description: Human-readable description
        tags: List of tags for categorization
    """
    def decorator(cls):
        # Step 1: Try to extract field decorators (new pattern)
        inputs_from_fields, outputs_from_fields, config_fields = _extract_field_decorators(cls)

        # Step 2: Try to extract nested classes (old pattern)
        inputs_from_nested, outputs_from_nested = _extract_nested_classes(cls)

        # Step 3: Try to extract method decorators (deprecated pattern)
        inputs_from_methods = {}
        outputs_from_methods = {}
        for attr_name in dir(cls):
            if attr_name.startswith('_'):
                continue
            try:
                attr = getattr(cls, attr_name)
            except AttributeError:
                continue

            # Check for old @input_port decorator
            if hasattr(attr, '__streamlib_input__'):
                port_info = attr.__streamlib_input__
                inputs_from_methods[port_info['name']] = port_info

            # Check for old @output_port decorator
            if hasattr(attr, '__streamlib_output__'):
                port_info = attr.__streamlib_output__
                outputs_from_methods[port_info['name']] = port_info

        # Step 4: Merge all patterns (priority: fields > nested > methods)
        inputs = {**inputs_from_methods, **inputs_from_nested, **inputs_from_fields}
        outputs = {**outputs_from_methods, **outputs_from_nested, **outputs_from_fields}

        # Step 5: Store metadata on class for Rust introspection
        cls.__streamlib_inputs__ = inputs
        cls.__streamlib_outputs__ = outputs
        cls.__streamlib_config_fields__ = config_fields
        cls.__streamlib_mode__ = mode
        cls.__streamlib_description__ = description
        cls.__streamlib_tags__ = tags or []

        # Step 6: Detect which pattern is being used
        if inputs_from_fields or outputs_from_fields:
            cls.__streamlib_pattern__ = 'field_decorators'
        elif inputs_from_nested or outputs_from_nested:
            cls.__streamlib_pattern__ = 'nested_classes'
        else:
            cls.__streamlib_pattern__ = 'method_decorators'

        return cls

    # Support both @StreamProcessor and @StreamProcessor(...) syntax
    if cls_or_mode is None:
        # Called with arguments: @StreamProcessor(mode="Pull", ...)
        return decorator
    elif isinstance(cls_or_mode, str):
        # Called with mode as first arg: @StreamProcessor("Pull")
        mode = cls_or_mode
        return decorator
    else:
        # Called without arguments: @StreamProcessor
        return decorator(cls_or_mode)
