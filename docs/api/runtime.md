# StreamRuntime

The `StreamRuntime` is the orchestrator that manages handler lifecycles, coordinates clock ticks, and negotiates capability-based connections.

## Philosophy

**The runtime is like a Unix shell** - it doesn't process data itself, but coordinates independent processes (handlers) and manages the pipes (connections) between them.

```bash
# Unix shell
cat file.txt | grep "error" | sed 's/ERROR/WARNING/' | awk '{print $1}'

# streamlib runtime
runtime.connect(camera.outputs['video'], blur.inputs['video'])
runtime.connect(blur.outputs['video'], display.inputs['video'])
runtime.start()
```

## Interface

```python
class StreamRuntime:
    """Manages handler lifecycle and clock distribution."""

    def __init__(self, fps: int = 30, gpu_backend: str = 'auto'):
        """Initialize runtime.

        Args:
            fps: Frames per second for clock ticks (default: 30)
            gpu_backend: GPU backend ('auto', 'metal', 'cuda', 'none')
        """
        self.fps = fps
        self.gpu_context = GPUContext(backend=gpu_backend)
        self._streams: List[Stream] = []
        self._running = False

    def add_stream(self, stream: Stream) -> None:
        """Add a handler to the runtime.

        Args:
            stream: Stream wrapping a handler with dispatcher config
        """
        ...

    def connect(
        self,
        output_port: StreamOutput,
        input_port: StreamInput,
        auto_transfer: bool = True
    ) -> None:
        """Connect output port to input port.

        Performs capability negotiation:
        1. Find common capabilities (cpu/gpu)
        2. If match found, use it
        3. If no match and auto_transfer=True, insert transfer handler
        4. Otherwise, raise error

        Args:
            output_port: Source port
            input_port: Destination port
            auto_transfer: Automatically insert CPU↔GPU transfers

        Raises:
            TypeError: If port types don't match (video → audio)
            ValueError: If no common capability and auto_transfer=False
        """
        ...

    def start(self) -> None:
        """Start the runtime (non-blocking).

        1. Activates all handlers
        2. Calls on_start() for each
        3. Starts clock
        4. Begins distributing ticks
        """
        ...

    def stop(self) -> None:
        """Stop the runtime.

        1. Stops clock
        2. Calls on_stop() for each handler
        3. Deactivates handlers
        4. Cleans up resources
        """
        ...
```

## Basic Usage

### Simple Pipeline

```python
import asyncio
from streamlib import StreamRuntime, Stream
from streamlib_extras import TestPatternHandler, DisplayGPUHandler

async def main():
    # Create runtime
    runtime = StreamRuntime(fps=30)

    # Create handlers
    pattern = TestPatternHandler(width=1920, height=1080, pattern='smpte_bars')
    display = DisplayGPUHandler(window_name='Test', width=1920, height=1080)

    # Add to runtime (dispatcher inferred automatically)
    runtime.add_stream(Stream(pattern))
    runtime.add_stream(Stream(display))

    # Connect
    runtime.connect(pattern.outputs['video'], display.inputs['video'])

    # Start
    runtime.start()

    # Run until interrupted
    try:
        while runtime._running:
            await asyncio.sleep(1)
    except KeyboardInterrupt:
        pass

    runtime.stop()

asyncio.run(main())
```

### Multi-Handler Pipeline

```python
from streamlib import StreamRuntime, Stream
from streamlib_extras import CameraHandlerGPU, BlurFilterGPU, DisplayGPUHandler

runtime = StreamRuntime(fps=30)

# Create handlers
camera = CameraHandlerGPU(device_name="Live Camera", width=1920, height=1080)
blur = BlurFilterGPU(kernel_size=15, sigma=8.0)
display = DisplayGPUHandler(width=1920, height=1080)

# Add to runtime (dispatcher inferred automatically)
runtime.add_stream(Stream(camera))
runtime.add_stream(Stream(blur))
runtime.add_stream(Stream(display))

# Connect pipeline
runtime.connect(camera.outputs['video'], blur.inputs['video'])
runtime.connect(blur.outputs['video'], display.inputs['video'])

# Start
runtime.start()
```

## Automatic GPU-First Architecture

The runtime automatically manages memory and execution:

### All-GPU Pipeline (Common)

```python
# All handlers are GPU-first by default
camera = CameraHandlerGPU()
blur = BlurFilterGPU()
display = DisplayGPUHandler()

runtime.connect(camera.outputs['video'], blur.inputs['video'])
runtime.connect(blur.outputs['video'], display.inputs['video'])
# ✅ All connections stay on GPU automatically
# No explicit capabilities needed!
```

### Automatic Transfer (When Needed)

```python
# GPU output → CPU-only handler (rare case)
camera = CameraHandlerGPU()
cpu_filter = CPUOnlyFilter(cpu_only=True)

runtime.connect(camera.outputs['video'], cpu_filter.inputs['video'])
# ✅ Runtime automatically inserts GPU→CPU transfer if needed
```

### Benefits

- **GPU by default** - All operations stay on GPU
- **Automatic execution** - Runtime infers optimal dispatcher
- **Zero-copy** - Data stays on GPU throughout pipeline
- **Simple API** - No explicit capabilities or dispatchers needed

## Stream Configuration

The `Stream` wrapper adds a handler to the runtime:

```python
from streamlib import Stream

# Runtime automatically infers execution context
runtime.add_stream(Stream(handler))

# Future: Set priority, CPU affinity, etc.
runtime.add_stream(Stream(handler, priority=10))
```

You don't need to specify dispatchers - the runtime handles this automatically based on the handler's operations.

## GPU Context

The runtime initializes a GPU context for all GPU-capable handlers:

```python
# Auto-detect backend
runtime = StreamRuntime(fps=30, gpu_backend='auto')
# → Detects Metal on macOS, CUDA on Linux/Windows

# Force specific backend
runtime = StreamRuntime(fps=30, gpu_backend='metal')

# Disable GPU
runtime = StreamRuntime(fps=30, gpu_backend='none')
```

All GPU handlers share this context for efficient resource usage.

## Clock System

The runtime maintains a central clock that drives all handlers:

```python
runtime = StreamRuntime(fps=30)  # 30 ticks per second
```

Every tick, the runtime broadcasts a `ClockTickEvent` with:
- `timestamp`: Monotonic time in seconds
- `frame_number`: Sequential frame counter

All handlers receive the same tick simultaneously (broadcast, not sequential).

## Lifecycle Events

```
runtime.start()
    ↓
    For each handler:
        1. _activate(runtime, event_bus, dispatcher)
        2. on_start()
    ↓
    Start clock
    ↓
    Every tick:
        Broadcast ClockTickEvent
        → All handlers process() called
    ↓
runtime.stop()
    ↓
    Stop clock
    ↓
    For each handler:
        1. on_stop()
        2. _deactivate()
```

## Multi-Input Handlers

Some handlers (like compositors) have multiple inputs:

```python
compositor = MultiInputCompositor(num_inputs=2)

# compositor.inputs = {'input_0': ..., 'input_1': ...}

runtime.connect(camera.outputs['video'], compositor.inputs['input_0'])
runtime.connect(pattern.outputs['video'], compositor.inputs['input_1'])
runtime.connect(compositor.outputs['video'], display.inputs['video'])
```

## Error Handling

The runtime catches errors in handler `process()` methods:

```python
# Handler raises error during process()
async def process(self, tick):
    raise RuntimeError("Something went wrong")

# Runtime catches and logs:
# [handler-id] Error processing tick 42: Something went wrong
# Handler continues processing next tick
```

To stop the runtime on error, raise in `on_start()` instead.

## Best Practices

1. **Create runtime once** - Reuse for entire application lifetime
2. **Add all handlers before connecting** - Makes connections visible
3. **Trust the runtime** - It automatically handles execution and memory
4. **Match FPS to slowest handler** - Don't set FPS higher than you can process
5. **Shut down cleanly** - Always call `runtime.stop()` before exit
6. **Let GPU do the work** - Runtime keeps data on GPU automatically

## Common Patterns

### Initialization Pattern

```python
async def main():
    runtime = StreamRuntime(fps=30)

    # Build pipeline
    handler1 = ...
    handler2 = ...
    runtime.add_stream(Stream(handler1))
    runtime.add_stream(Stream(handler2))
    runtime.connect(handler1.outputs['video'], handler2.inputs['video'])

    # Start
    runtime.start()

    # Run
    try:
        while runtime._running:
            await asyncio.sleep(1)
    except KeyboardInterrupt:
        print("Stopping...")

    # Cleanup
    runtime.stop()

asyncio.run(main())
```

### Multiple Pipelines

```python
# Pipeline 1: Camera → Blur
runtime.connect(camera.outputs['video'], blur.inputs['video'])

# Pipeline 2: Test Pattern (independent)
runtime.add_stream(Stream(pattern))

# Merge pipelines: Compositor combines both
runtime.connect(blur.outputs['video'], compositor.inputs['input_0'])
runtime.connect(pattern.outputs['video'], compositor.inputs['input_1'])
runtime.connect(compositor.outputs['video'], display.inputs['video'])
```

## See Also

- [StreamHandler](handler.md) - Handler interface
- [Ports](ports.md) - Input/Output port system
- [Composition Guide](../guides/composition.md) - Build complex pipelines
- [Dispatcher Guidelines](../../docs/dispatcher-guidelines.md) - Choose the right dispatcher
