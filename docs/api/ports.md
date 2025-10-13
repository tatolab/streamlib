# Ports

Ports are the connection points between handlers. Like Unix pipes, they define how data flows through your pipeline.

## Philosophy

**Ports declare capabilities, runtime negotiates memory space.**

Instead of hardcoding CPU or GPU, handlers declare what they *can* support, and the runtime finds the optimal path:

```python
# Traditional approach (rigid)
video_input = VideoInput(format='RGB', memory='GPU')  # ❌ Too specific

# streamlib approach (flexible)
video_input = VideoInput('video', capabilities=['cpu', 'gpu'])  # ✅ Runtime decides
```

## Port Types

### StreamInput

Receives data from connected output ports.

```python
class StreamInput:
    """Input port for receiving data."""

    def __init__(
        self,
        name: str,
        port_type: str,
        capabilities: List[str]
    ):
        """Create input port.

        Args:
            name: Port identifier
            port_type: Data type ('video', 'audio', 'data')
            capabilities: Supported memory spaces ['cpu'], ['gpu'], or ['cpu', 'gpu']
        """
        self.name = name
        self.port_type = port_type
        self.capabilities = capabilities
        self.negotiated_memory: Optional[str] = None
        self._buffer: Optional[RingBuffer] = None

    def connect(self, buffer: RingBuffer) -> None:
        """Connect to a ring buffer (called by runtime)."""
        self._buffer = buffer

    def read_latest(self) -> Optional[Any]:
        """Read most recent item from buffer."""
        if not self._buffer:
            return None
        return self._buffer.read_latest()

    def read_all(self) -> List[Any]:
        """Read all available items."""
        if not self._buffer:
            return []
        return self._buffer.read_all()
```

### StreamOutput

Sends data to connected input ports.

```python
class StreamOutput:
    """Output port for sending data."""

    def __init__(
        self,
        name: str,
        port_type: str,
        capabilities: List[str],
        slots: int = 3
    ):
        """Create output port.

        Args:
            name: Port identifier
            port_type: Data type ('video', 'audio', 'data')
            capabilities: Supported memory spaces ['cpu'], ['gpu'], or ['cpu', 'gpu']
            slots: Ring buffer size (default: 3)
        """
        self.name = name
        self.port_type = port_type
        self.capabilities = capabilities
        self.negotiated_memory: Optional[str] = None
        self.buffer = RingBuffer(slots=slots)

    def write(self, item: Any) -> None:
        """Write item to buffer."""
        self.buffer.write(item)
```

## Helper Functions

For common port types:

```python
def VideoInput(name: str, capabilities: List[str]) -> StreamInput:
    """Create a video input port."""
    return StreamInput(name, port_type='video', capabilities=capabilities)

def VideoOutput(name: str, capabilities: List[str], slots: int = 3) -> StreamOutput:
    """Create a video output port."""
    return StreamOutput(name, port_type='video', capabilities=capabilities, slots=slots)

def AudioInput(name: str, capabilities: List[str]) -> StreamInput:
    """Create an audio input port."""
    return StreamInput(name, port_type='audio', capabilities=capabilities)

def AudioOutput(name: str, capabilities: List[str], slots: int = 3) -> StreamOutput:
    """Create an audio output port."""
    return StreamOutput(name, port_type='audio', capabilities=capabilities, slots=slots)
```

## Capabilities

Capabilities declare which memory spaces a handler supports:

### CPU-Only

```python
# Handler only works with CPU memory
self.inputs['video'] = VideoInput('video', capabilities=['cpu'])
self.outputs['video'] = VideoOutput('video', capabilities=['cpu'])
```

Use for:
- Pure Python/NumPy processing
- Legacy CPU-only libraries
- Simple transformations

### GPU-Only

```python
# Handler only works with GPU memory
self.inputs['video'] = VideoInput('video', capabilities=['gpu'])
self.outputs['video'] = VideoOutput('video', capabilities=['gpu'])
```

Use for:
- GPU-only operations (Metal shaders, CUDA kernels)
- When CPU fallback would be too slow

### Flexible (CPU + GPU)

```python
# Handler works with both
self.inputs['video'] = VideoInput('video', capabilities=['cpu', 'gpu'])
self.outputs['video'] = VideoOutput('video', capabilities=['gpu'])
```

Use for:
- Handlers with GPU fast path, CPU fallback
- Maximum composability
- **Recommended default** for new handlers

## Negotiation

When connecting ports, the runtime finds a common capability:

### Scenario 1: Perfect Match

```python
# Both support GPU
output: capabilities=['gpu']
input:  capabilities=['gpu']

runtime.connect(output, input)
# ✅ Negotiated: gpu
# Both ports set: negotiated_memory = 'gpu'
```

### Scenario 2: Multiple Options

```python
# Both support CPU and GPU
output: capabilities=['cpu', 'gpu']
input:  capabilities=['cpu', 'gpu']

runtime.connect(output, input)
# ✅ Negotiated: gpu (runtime prefers GPU)
# Both ports set: negotiated_memory = 'gpu'
```

### Scenario 3: Single Match

```python
# Only CPU in common
output: capabilities=['cpu', 'gpu']
input:  capabilities=['cpu']

runtime.connect(output, input)
# ✅ Negotiated: cpu
# Both ports set: negotiated_memory = 'cpu'
```

### Scenario 4: No Match + Auto-Transfer

```python
# No common capability, but auto_transfer=True
output: capabilities=['gpu']
input:  capabilities=['cpu']

runtime.connect(output, input, auto_transfer=True)
# ✅ Runtime inserts GPU→CPU transfer handler
# Connection succeeds
```

### Scenario 5: Type Mismatch

```python
# Different port types
video_output: port_type='video'
audio_input:  port_type='audio'

runtime.connect(video_output, audio_input)
# ❌ TypeError: Cannot connect video port to audio port
```

## Reading from Ports

### read_latest()

Get the most recent item (common for real-time video):

```python
async def process(self, tick: TimedTick):
    frame = self.inputs['video'].read_latest()
    if frame:
        # Process most recent frame
        ...
```

### read_all()

Get all unread items (useful for batching):

```python
async def process(self, tick: TimedTick):
    frames = self.inputs['video'].read_all()
    for frame in frames:
        # Process each frame
        ...
```

## Writing to Ports

### write()

Add an item to the output buffer:

```python
async def process(self, tick: TimedTick):
    frame = self.inputs['video'].read_latest()
    if frame:
        # Process frame
        processed = do_something(frame)
        # Write to output
        self.outputs['video'].write(processed)
```

## Port Naming

Convention: Use descriptive names that indicate purpose

```python
# Single input/output
self.inputs['video'] = VideoInput('video', ...)
self.outputs['video'] = VideoOutput('video', ...)

# Multiple inputs (compositor)
self.inputs['input_0'] = VideoInput('input_0', ...)
self.inputs['input_1'] = VideoInput('input_1', ...)
self.outputs['video'] = VideoOutput('video', ...)

# Specific purpose
self.inputs['foreground'] = VideoInput('foreground', ...)
self.inputs['background'] = VideoInput('background', ...)
self.outputs['composited'] = VideoOutput('composited', ...)
```

## Ring Buffers

Output ports use ring buffers to store recent items:

```python
# Default: 3 slots
self.outputs['video'] = VideoOutput('video', capabilities=['gpu'], slots=3)

# Larger buffer for bursty producers
self.outputs['video'] = VideoOutput('video', capabilities=['gpu'], slots=10)

# Minimal buffer for tight memory
self.outputs['video'] = VideoOutput('video', capabilities=['gpu'], slots=1)
```

**How it works:**
- Writer adds items with `write()`
- Buffer keeps last N items
- Reader gets items with `read_latest()` or `read_all()`
- Old items automatically dropped when buffer fills

## Complete Examples

### Simple Pass-Through

```python
class PassThroughHandler(StreamHandler):
    def __init__(self):
        super().__init__()
        # Flexible: accepts CPU or GPU
        self.inputs['video'] = VideoInput('video', capabilities=['cpu', 'gpu'])
        self.outputs['video'] = VideoOutput('video', capabilities=['cpu', 'gpu'])

    async def process(self, tick: TimedTick):
        frame = self.inputs['video'].read_latest()
        if frame:
            self.outputs['video'].write(frame)
```

### GPU-Accelerated Filter

```python
class BlurFilterGPU(StreamHandler):
    def __init__(self):
        super().__init__()
        # GPU-only
        self.inputs['video'] = VideoInput('video', capabilities=['gpu'])
        self.outputs['video'] = VideoOutput('video', capabilities=['gpu'])

    async def process(self, tick: TimedTick):
        frame = self.inputs['video'].read_latest()
        if frame:
            # Process on GPU
            blurred = self.gpu_blur(frame.data)
            self.outputs['video'].write(VideoFrame(
                data=blurred,
                timestamp=frame.timestamp,
                frame_number=frame.frame_number,
                width=frame.width,
                height=frame.height
            ))
```

### Multi-Input Compositor

```python
class CompositorHandler(StreamHandler):
    def __init__(self, num_inputs: int = 2):
        super().__init__()
        # Multiple inputs
        for i in range(num_inputs):
            self.inputs[f'input_{i}'] = VideoInput(
                f'input_{i}',
                capabilities=['gpu', 'cpu']
            )
        # Single output
        self.outputs['video'] = VideoOutput('video', capabilities=['gpu'])

    async def process(self, tick: TimedTick):
        # Read from all inputs
        frames = []
        for i in range(self.num_inputs):
            frame = self.inputs[f'input_{i}'].read_latest()
            if frame:
                frames.append(frame)

        if len(frames) == self.num_inputs:
            # Composite when all inputs ready
            composited = self.composite(frames)
            self.outputs['video'].write(composited)
```

## Best Practices

1. **Default to flexible capabilities** - Use `['cpu', 'gpu']` unless you have a reason not to
2. **Let runtime negotiate** - Don't assume which memory space will be used
3. **Check negotiated_memory in on_start()** - Set up resources based on actual negotiated space
4. **Use read_latest() for real-time** - Don't process old frames
5. **Size buffers appropriately** - Default of 3 works for most cases

## See Also

- [StreamHandler](handler.md) - Handler interface
- [Runtime](runtime.md) - Connection and negotiation
- [Messages](messages.md) - VideoFrame and other data types
