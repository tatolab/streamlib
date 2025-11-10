"""
Test Python decorators without loading the binary extension.

This tests just the decorator functionality defined in __init__.py.
"""

# Import only the decorator code (not the binary .so)
import sys
sys.path.insert(0, 'libs/streamlib/python')

# Manually define the decorators and types (copied from __init__.py)
class VideoFrame:
    """Video frame type marker for port declarations"""
    pass


def input_port(name: str, type: type, required: bool = True):
    """Decorator to mark a method as an input port."""
    def decorator(func):
        func.__streamlib_input__ = {
            'name': name,
            'type': type.__name__ if hasattr(type, '__name__') else str(type),
            'required': required,
            'channels': getattr(type, 'channels', None) if type.__name__ == 'AudioFrame' else None,
        }
        return func
    return decorator


def output_port(name: str, type: type):
    """Decorator to mark a method as an output port."""
    def decorator(func):
        func.__streamlib_output__ = {
            'name': name,
            'type': type.__name__ if hasattr(type, '__name__') else str(type),
            'channels': getattr(type, 'channels', None) if type.__name__ == 'AudioFrame' else None,
        }
        return func
    return decorator


def StreamProcessor(cls):
    """Class decorator that collects all port metadata from decorated methods."""
    inputs = {}
    outputs = {}

    for attr_name in dir(cls):
        if attr_name.startswith('_'):
            continue

        try:
            attr = getattr(cls, attr_name)
        except AttributeError:
            continue

        if hasattr(attr, '__streamlib_input__'):
            port_info = attr.__streamlib_input__
            inputs[port_info['name']] = port_info

        if hasattr(attr, '__streamlib_output__'):
            port_info = attr.__streamlib_output__
            outputs[port_info['name']] = port_info

    cls.__streamlib_inputs__ = inputs
    cls.__streamlib_outputs__ = outputs

    return cls


# Test the decorators
@StreamProcessor
class TestProcessor:
    @input_port(name="video_in", type=VideoFrame, required=True)
    def video_in(self):
        return None

    @output_port(name="video_out", type=VideoFrame)
    def video_out(self):
        return None


# Verify metadata
print("✅ Testing Python decorator pattern...\n")

print(f"TestProcessor inputs: {TestProcessor.__streamlib_inputs__}")
print(f"TestProcessor outputs: {TestProcessor.__streamlib_outputs__}\n")

# Verify structure
assert 'video_in' in TestProcessor.__streamlib_inputs__, "Input port not found"
assert 'video_out' in TestProcessor.__streamlib_outputs__, "Output port not found"

input_meta = TestProcessor.__streamlib_inputs__['video_in']
assert input_meta['name'] == 'video_in', "Input port name mismatch"
assert input_meta['type'] == 'VideoFrame', "Input port type mismatch"
assert input_meta['required'] == True, "Input port required mismatch"

output_meta = TestProcessor.__streamlib_outputs__['video_out']
assert output_meta['name'] == 'video_out', "Output port name mismatch"
assert output_meta['type'] == 'VideoFrame', "Output port type mismatch"

print("✅ All decorator tests passed!")
print("\nRust will be able to introspect these attributes:")
print(f"  - python_class.getattr('__streamlib_inputs__') -> {TestProcessor.__streamlib_inputs__}")
print(f"  - python_class.getattr('__streamlib_outputs__') -> {TestProcessor.__streamlib_outputs__}")
