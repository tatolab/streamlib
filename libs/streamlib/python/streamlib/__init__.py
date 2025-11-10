"""streamlib - Real-time streaming infrastructure for AI agents"""

from .streamlib import *

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


# Port decorators for declaring processor inputs and outputs
def input_port(name: str, type: type, required: bool = True):
    """
    Decorator to mark a method as an input port.

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
    Decorator to mark a method as an output port.

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


def StreamProcessor(cls):
    """
    Class decorator that collects all port metadata from decorated methods.

    Scans the class for @input_port and @output_port decorated methods and
    stores the metadata in __streamlib_inputs__ and __streamlib_outputs__
    attributes for Rust introspection.

    Usage:
        @StreamProcessor
        class MyProcessor:
            @input_port(name="video", type=VideoFrame)
            def video(self):
                return self._video

            @output_port(name="output", type=VideoFrame)
            def output(self):
                return self._output

            def process(self):
                # Processing logic here
                pass
    """
    inputs = {}
    outputs = {}

    # Scan class methods for port decorators
    for attr_name in dir(cls):
        # Skip private and built-in attributes
        if attr_name.startswith('_'):
            continue

        try:
            attr = getattr(cls, attr_name)
        except AttributeError:
            continue

        # Check for input port decorator
        if hasattr(attr, '__streamlib_input__'):
            port_info = attr.__streamlib_input__
            inputs[port_info['name']] = port_info

        # Check for output port decorator
        if hasattr(attr, '__streamlib_output__'):
            port_info = attr.__streamlib_output__
            outputs[port_info['name']] = port_info

    # Store metadata on class for Rust introspection
    cls.__streamlib_inputs__ = inputs
    cls.__streamlib_outputs__ = outputs

    return cls
