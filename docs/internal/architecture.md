# streamlib Architecture

## Purpose

**streamlib is a Python SDK for building realtime stream processing applications.**

Target users: broadcasters, app developers, ML engineers building realtime video/audio/data pipelines.

## Design Philosophy

### SDK, Not Framework

Like **PyTorch core** (provides tensors, not pre-built models) or **Cloudflare Actors** (provides runtime, not applications):

- ✅ **We provide**: Core abstractions (StreamHandler, Runtime, RingBuffer, Clock)
- ✅ **Users build**: Their own handlers with their own libraries
- ✅ **Zero dependencies**: Core SDK uses only Python stdlib
- ✅ **Great DX**: Simple API, powerful composition, type hints everywhere

### Unix Philosophy for Streams

Like `grep | sed | awk` but for realtime streams:

```python
# Text processing (Unix)
cat file.txt | grep "error" | sed 's/ERROR/WARNING/'

# Stream processing (streamlib)
runtime.connect(source.outputs['video'], processor.inputs['video'])
runtime.connect(processor.outputs['video'], sink.inputs['video'])
```

Each handler: single purpose, composable, independent.

### Broadcast-Quality Requirements

**Zero-copy architecture:**
- Ring buffers hold references, not data
- GPU tensors stay on GPU throughout pipeline
- No CPU↔GPU transfers except at boundaries
- Capability negotiation handles memory space transitions

**Clock-driven realtime:**
- Fixed-rate ticks drive processing
- Automatic frame dropping when can't keep up
- No backpressure, no queueing
- Predictable, deterministic timing

**Professional timing:**
- PTP (Precision Time Protocol) support
- Genlock hardware sync
- SMPTE ST 2110 alignment
- Multi-camera synchronization

---

## Core Architecture

### Three-Layer Design

```
StreamRuntime (lifecycle, clock, dispatchers, supervision, negotiation)
    ↓
    Manages multiple Streams
    ↓
Stream (config: handler + dispatcher + transport [optional])
    ↓
    Wraps StreamHandler
    ↓
StreamHandler (processing logic)
    ↓
    inputs/outputs → Capability-Based Ports → RingBuffers (zero-copy references)
```

**StreamRuntime** = Cloudflare Wrangler (manages lifecycle + negotiates capabilities)
**Stream** = Configuration wrapper (handler + dispatcher + transport)
**StreamHandler** = Durable Object (inert until activated)

---

## StreamHandler (Processing Logic)

**Base class for all stream processing.**

```python
from abc import ABC, abstractmethod
from typing import Dict, List, Optional

class StreamHandler(ABC):
    """
    Base class for stream processing handlers.

    Handlers are pure processing logic - reusable across execution contexts.
    Runtime manages lifecycle, clock, and dispatcher assignment.

    Handlers are INERT until added to StreamRuntime.
    """

    def __init__(self, handler_id: str = None):
        self.handler_id = handler_id or f"{self.__class__.__name__}-{id(self)}"

        # Input/output ports (backed by ring buffers)
        # Ports declare capabilities (what memory spaces they support)
        self.inputs: Dict[str, StreamInput] = {}
        self.outputs: Dict[str, StreamOutput] = {}

        # Runtime-managed (set by runtime)
        self._runtime = None
        self._clock = None
        self._dispatcher = None
        self._running = False

    @abstractmethod
    async def process(self, tick: TimedTick) -> None:
        """
        Process one clock tick.

        1. Read latest data from inputs (zero-copy)
        2. Process data (check negotiated_memory if handler adapts)
        3. Write results to outputs (zero-copy)

        Example:
            async def process(self, tick: TimedTick):
                frame = self.inputs['video'].read_latest()  # Zero-copy reference
                if frame:
                    # Check negotiated memory space if handler adapts
                    if self.inputs['video'].negotiated_memory == 'gpu':
                        result = self.gpu_process(frame)  # GPU tensor stays on GPU
                    else:
                        result = self.cpu_process(frame)  # CPU numpy array
                    self.outputs['video'].write(result)  # Zero-copy write
        """
        pass

    # Optional lifecycle hooks
    async def on_start(self) -> None:
        """Called once when runtime starts this handler."""
        pass

    async def on_stop(self) -> None:
        """Called once when runtime stops this handler."""
        pass
```

**Key points:**
- Handlers are **pure processing logic** (no lifecycle awareness)
- **Inert until runtime starts them** (no auto-start)
- **Reusable** across different dispatchers
- **Zero-copy** ring buffer reads/writes
- **Clock-driven** via `process(tick)` calls
- **Capability-aware** ports for CPU/GPU flexibility

---

## Capability-Based Ports (GStreamer-Inspired)

**Ports declare what memory spaces they CAN work with.**

```python
class StreamOutput:
    """
    Output port with capability negotiation.

    Capabilities list memory spaces this port can produce:
    - ['cpu'] - CPU memory only (numpy arrays)
    - ['gpu'] - GPU memory only (torch tensors)
    - ['cpu', 'gpu'] - Flexible, can produce either
    """

    def __init__(
        self,
        name: str,
        port_type: str,  # 'video', 'audio', 'data'
        capabilities: List[str],  # ['cpu'], ['gpu'], or ['cpu', 'gpu']
        slots: int = 3
    ):
        self.name = name
        self.port_type = port_type
        self.capabilities = capabilities
        self.buffer = RingBuffer(slots=slots)
        self.negotiated_memory: Optional[str] = None  # Set during connect

    def write(self, data) -> None:
        """Write reference to ring buffer (zero-copy)."""
        self.buffer.write(data)


class StreamInput:
    """
    Input port with capability negotiation.

    Capabilities list memory spaces this port can accept:
    - ['cpu'] - CPU memory only
    - ['gpu'] - GPU memory only
    - ['cpu', 'gpu'] - Flexible, can accept either
    """

    def __init__(
        self,
        name: str,
        port_type: str,  # 'video', 'audio', 'data'
        capabilities: List[str]  # ['cpu'], ['gpu'], or ['cpu', 'gpu']
    ):
        self.name = name
        self.port_type = port_type
        self.capabilities = capabilities
        self.buffer: Optional[RingBuffer] = None
        self.negotiated_memory: Optional[str] = None  # Set during connect

    def connect(self, buffer: RingBuffer) -> None:
        """Connect to ring buffer."""
        self.buffer = buffer

    def read_latest(self):
        """Read latest reference (zero-copy)."""
        if self.buffer is None:
            return None
        return self.buffer.read_latest()


# Convenience factory functions
def VideoInput(name: str, capabilities: List[str] = ['cpu']) -> StreamInput:
    """Create video input port with capabilities."""
    return StreamInput(name, port_type='video', capabilities=capabilities)

def VideoOutput(name: str, capabilities: List[str] = ['cpu'], slots: int = 3) -> StreamOutput:
    """Create video output port with capabilities."""
    return StreamOutput(name, port_type='video', capabilities=capabilities, slots=slots)

def AudioInput(name: str, capabilities: List[str] = ['cpu']) -> StreamInput:
    """Create audio input port with capabilities."""
    return StreamInput(name, port_type='audio', capabilities=capabilities)

def AudioOutput(name: str, capabilities: List[str] = ['cpu'], slots: int = 3) -> StreamOutput:
    """Create audio output port with capabilities."""
    return StreamOutput(name, port_type='audio', capabilities=capabilities, slots=slots)
```

---

## Capability Negotiation (Runtime)

**Runtime negotiates compatible memory space when connecting ports.**

```python
class StreamRuntime:
    def connect(
        self,
        output_port: StreamOutput,
        input_port: StreamInput,
        auto_transfer: bool = True
    ) -> None:
        """
        Connect output to input with capability negotiation.

        Negotiation rules:
        1. Port types must match (video→video, audio→audio)
        2. Find intersection of capabilities
        3. If intersection exists, negotiate that memory space
        4. If no intersection, auto-insert transfer handler (or error)
        """

        # Check port type compatibility
        if output_port.port_type != input_port.port_type:
            raise TypeError(
                f"Cannot connect {output_port.port_type} output to "
                f"{input_port.port_type} input"
            )

        # Find capability intersection
        common_caps = set(output_port.capabilities) & set(input_port.capabilities)

        if common_caps:
            # Negotiate: prefer upstream's actual memory space
            # (minimizes transfers, lets upstream decide)
            negotiated = output_port.capabilities[0]  # Prefer first declared

            output_port.negotiated_memory = negotiated
            input_port.negotiated_memory = negotiated
            input_port.connect(output_port.buffer)

            print(f"✅ Connected {output_port.name} → {input_port.name} "
                  f"(negotiated: {negotiated})")
        else:
            # No common capabilities - need transfer handler
            if not auto_transfer:
                raise TypeError(
                    f"Memory space mismatch: output supports {output_port.capabilities}, "
                    f"input supports {input_port.capabilities}. "
                    f"Use explicit transfer handler or set auto_transfer=True"
                )

            # Auto-insert transfer handler
            self._insert_transfer_handler(output_port, input_port)

    def _insert_transfer_handler(
        self,
        output_port: StreamOutput,
        input_port: StreamInput
    ) -> None:
        """Auto-insert transfer handler between incompatible ports."""

        # Determine transfer direction
        out_mem = output_port.capabilities[0]
        in_mem = input_port.capabilities[0]

        print(
            f"⚠️  WARNING: Auto-inserting {out_mem}→{in_mem} transfer "
            f"for {output_port.port_type} (performance cost ~2ms). "
            f"Consider explicit placement for control."
        )

        # Create appropriate transfer handler
        if out_mem == 'cpu' and in_mem == 'gpu':
            transfer = CPUtoGPUTransferHandler()
            dispatcher = 'gpu'
        elif out_mem == 'gpu' and in_mem == 'cpu':
            transfer = GPUtoCPUTransferHandler()
            dispatcher = 'asyncio'
        else:
            raise ValueError(f"Unknown transfer: {out_mem}→{in_mem}")

        # Add transfer handler to runtime
        transfer_stream = Stream(transfer, dispatcher=dispatcher)
        self.add_stream(transfer_stream)

        # Wire: output → transfer → input
        transfer.inputs['in'].connect(output_port.buffer)
        input_port.connect(transfer.outputs['out'].buffer)

        # Set negotiated memory spaces
        output_port.negotiated_memory = out_mem
        transfer.inputs['in'].negotiated_memory = out_mem
        transfer.outputs['out'].negotiated_memory = in_mem
        input_port.negotiated_memory = in_mem

        self._transfer_handlers.append(transfer)
```

---

## Transfer Handlers

**Explicit handlers for CPU↔GPU memory transfers.**

```python
class CPUtoGPUTransferHandler(StreamHandler):
    """Transfer video frames from CPU to GPU memory."""

    def __init__(self, device='cuda:0'):
        super().__init__()
        self.device = device
        # Input: CPU only, Output: GPU only
        self.inputs['in'] = VideoInput('in', capabilities=['cpu'])
        self.outputs['out'] = VideoOutput('out', capabilities=['gpu'])

    async def process(self, tick: TimedTick):
        frame = self.inputs['in'].read_latest()
        if frame:
            # Explicit CPU → GPU transfer
            gpu_data = torch.from_numpy(frame.data).to(self.device)
            self.outputs['out'].write(VideoFrame(gpu_data, frame.timestamp, ...))


class GPUtoCPUTransferHandler(StreamHandler):
    """Transfer video frames from GPU to CPU memory."""

    def __init__(self):
        super().__init__()
        # Input: GPU only, Output: CPU only
        self.inputs['in'] = VideoInput('in', capabilities=['gpu'])
        self.outputs['out'] = VideoOutput('out', capabilities=['cpu'])

    async def process(self, tick: TimedTick):
        frame = self.inputs['in'].read_latest()
        if frame:
            # Explicit GPU → CPU transfer
            cpu_data = frame.data.cpu().numpy()
            self.outputs['out'].write(VideoFrame(cpu_data, frame.timestamp, ...))
```

---

## Stream (Configuration Wrapper)

**Wraps handler with dispatcher and optional transport.**

```python
class Stream:
    """
    Configuration wrapper for StreamHandler.

    Stream = Handler + Dispatcher + Transport (optional)

    Dispatcher determines execution context:
    - 'asyncio' - AsyncioDispatcher (I/O-bound, default)
    - 'threadpool' - ThreadPoolDispatcher (CPU-bound)
    - 'gpu' - GPUDispatcher (GPU-accelerated)
    - 'processpool' - ProcessPoolDispatcher (heavy compute)
    - Or pass custom Dispatcher instance

    Transport is optional (only for I/O handlers):
    - Internal processing handlers don't need transport
    - I/O handlers (camera, display, network) use transport for metadata
    """

    def __init__(
        self,
        handler: StreamHandler,
        dispatcher: str | Dispatcher = 'asyncio',  # Dispatcher name or instance
        transport: Optional[Dict] = None,  # Optional, for I/O handlers only

        # Lifecycle policies (Phase 4)
        restart_policy: str = 'never',  # 'never', 'on-failure', 'always'
        concurrency_limit: Optional[int] = None,
        time_limit: Optional[int] = None,
        **kwargs
    ):
        self.handler = handler
        self.dispatcher = dispatcher
        self.transport = transport
        self.config = {
            'dispatcher': dispatcher,
            'transport': transport,
            'restart_policy': restart_policy,
            'concurrency_limit': concurrency_limit,
            'time_limit': time_limit,
            **kwargs
        }
```

**Transport examples (optional, metadata only):**
```python
# Internal processing (no transport)
filter_stream = Stream(FilterHandler(), dispatcher='asyncio')

# Camera I/O (transport for metadata/registry)
camera_stream = Stream(
    CameraHandler(device_id=0),
    dispatcher='asyncio',
    transport={'type': 'camera', 'device': 0}
)

# Display I/O
display_stream = Stream(
    DisplayHandler(window='preview'),
    dispatcher='asyncio',
    transport={'type': 'display', 'window': 'preview'}
)

# RTP network I/O (Phase 4)
rtp_stream = Stream(
    RTPSendHandler(host='239.0.0.1', port=5004),
    dispatcher='asyncio',
    transport={'type': 'rtp', 'host': '239.0.0.1', 'port': 5004}
)
```

---

## StreamRuntime (Lifecycle Manager)

**Central runtime that manages all handlers.**

```python
class StreamRuntime:
    """
    Runtime for managing StreamHandler lifecycle.

    Inspired by Cloudflare Wrangler + GStreamer capability negotiation.

    Responsibilities:
    - Provide shared clock for all handlers
    - Manage dispatcher pool
    - Start/stop handlers
    - Negotiate port capabilities and insert transfer handlers
    - Supervise handlers (Phase 4: restart policies)
    - Manage flat handler registry
    """

    def __init__(
        self,
        fps: float = 60.0,
        clock: Optional[Clock] = None
    ):
        self.fps = fps
        self.clock = clock or SoftwareClock(fps=fps)

        # Flat handler registry (all handlers are siblings)
        self.handlers: Dict[str, StreamHandler] = {}
        self.streams: Dict[str, Stream] = {}

        # Dispatcher pool (reusable dispatchers)
        self.dispatchers: Dict[str, Dispatcher] = {
            'asyncio': AsyncioDispatcher(),
            'threadpool': ThreadPoolDispatcher(workers=4),
            'gpu': GPUDispatcher(device='cuda:0'),
            'processpool': ProcessPoolDispatcher(workers=2),
        }

        # Track auto-inserted transfer handlers
        self._transfer_handlers: List[StreamHandler] = []

    def add_stream(self, stream: Stream) -> None:
        """
        Add stream to runtime.

        Handler remains inert until runtime.start().
        """
        handler = stream.handler
        handler._runtime = self
        handler._clock = self.clock

        # Get dispatcher from pool or use custom instance
        if isinstance(stream.dispatcher, str):
            handler._dispatcher = self.dispatchers[stream.dispatcher]
        else:
            handler._dispatcher = stream.dispatcher

        # Register in flat registry
        self.handlers[handler.handler_id] = handler
        self.streams[handler.handler_id] = stream

    async def start(self) -> None:
        """
        Start runtime and all handlers.

        - Starts shared clock
        - Spawns handler tasks
        - Begins supervision (Phase 4)
        """
        # Call on_start hooks
        for handler in self.handlers.values():
            await handler.on_start()

        # Spawn handler tasks (clock-driven)
        for handler in self.handlers.values():
            handler._running = True
            task = asyncio.create_task(self._run_handler(handler))

    async def _run_handler(self, handler: StreamHandler) -> None:
        """Run handler processing loop (clock-driven)."""
        while handler._running:
            tick = await self.clock.next_tick()

            try:
                # Schedule via dispatcher
                await handler._dispatcher.schedule(handler.process(tick))
            except Exception as e:
                # Phase 4: Handle restart policy
                # For now: crash runtime
                raise
```

---

## Ring Buffers (Zero-Copy)

**Fixed-size circular buffers with latest-read semantics.**

```python
from typing import Generic, TypeVar, Optional
import threading

T = TypeVar('T')

class RingBuffer(Generic[T]):
    """
    Fixed-size circular buffer (3 slots, broadcast standard).

    Latest-read semantics:
    - No queueing, no backpressure
    - Always get most recent data
    - Old data automatically skipped

    Zero-copy:
    - Stores references, not data copies
    - GPU tensors stay on GPU
    - CPU arrays stay in place
    """

    def __init__(self, slots: int = 3):
        self.slots = slots
        self.buffer: List[Optional[T]] = [None] * slots
        self.write_idx = 0
        self.lock = threading.Lock()
        self.has_data = False

    def write(self, data: T) -> None:
        """Write reference to ring buffer (zero-copy)."""
        with self.lock:
            self.buffer[self.write_idx] = data  # Just store reference
            self.write_idx = (self.write_idx + 1) % self.slots
            self.has_data = True

    def read_latest(self) -> Optional[T]:
        """Read latest reference from ring buffer (zero-copy)."""
        with self.lock:
            if not self.has_data:
                return None
            idx = (self.write_idx - 1) % self.slots
            return self.buffer[idx]  # Return reference, not copy
```

**Why 3 slots?**
- Matches professional broadcast practice
- Writer, reader, and one spare
- Minimal latency, minimal memory

**GPU ring buffers (pre-allocated):**
```python
class GPURingBuffer:
    """Pre-allocated GPU memory ring buffer."""

    def __init__(self, slots: int = 3, shape: tuple = (1920, 1080, 3)):
        # Pre-allocate GPU buffers (avoid malloc during runtime)
        self.buffers = [
            torch.zeros(shape, device='cuda', dtype=torch.uint8)
            for _ in range(slots)
        ]
        self.write_idx = 0
        self.lock = threading.Lock()

    def get_write_buffer(self) -> torch.Tensor:
        """Get GPU buffer to write into (zero-copy)."""
        with self.lock:
            return self.buffers[self.write_idx]

    def advance(self) -> None:
        """Mark current buffer as ready."""
        with self.lock:
            self.write_idx = (self.write_idx + 1) % len(self.buffers)

    def get_read_buffer(self) -> torch.Tensor:
        """Get latest GPU buffer (zero-copy)."""
        with self.lock:
            idx = (self.write_idx - 1) % len(self.buffers)
            return self.buffers[idx]
```

---

## Clock Abstraction

**Swappable clock sources for professional timing.**

```python
from dataclasses import dataclass

@dataclass
class TimedTick:
    """Clock tick with timing metadata."""
    timestamp: float      # PTP or wall clock time
    frame_number: int     # Monotonic frame counter
    clock_source_id: str  # Clock ID for sync
    fps: float           # Clock rate

class Clock(ABC):
    """Abstract clock interface."""

    @abstractmethod
    async def next_tick(self) -> TimedTick:
        """Wait for next tick."""
        pass

    @abstractmethod
    def get_fps(self) -> float:
        """Get nominal frame rate."""
        pass

# Implementations:
# - SoftwareClock(fps=60) - Free-running software timer
# - PTPClock(ptp_client) - IEEE 1588 Precision Time Protocol
# - GenlockClock(sdi_device) - Hardware sync (SDI genlock)
```

**Runtime provides single clock for entire session:**
- All handlers tick at same rate
- Synchronized processing
- Multi-camera alignment
- Professional broadcast timing

---

## Dispatchers (Execution Contexts)

**Four dispatcher types for different workload characteristics.**

```python
class Dispatcher(ABC):
    """Abstract dispatcher for handler execution."""

    @abstractmethod
    async def schedule(self, coro):
        """Execute handler coroutine in appropriate context."""
        pass

# 1. AsyncioDispatcher - I/O-bound (network, file, display)
class AsyncioDispatcher(Dispatcher):
    async def schedule(self, coro):
        await coro  # Direct execution on event loop

# 2. ThreadPoolDispatcher - CPU-bound (encoding, audio DSP)
class ThreadPoolDispatcher(Dispatcher):
    def __init__(self, workers: int = 4):
        self.executor = ThreadPoolExecutor(max_workers=workers)

    async def schedule(self, coro):
        loop = asyncio.get_running_loop()
        await loop.run_in_executor(self.executor,
                                    lambda: asyncio.run(coro))

# 3. ProcessPoolDispatcher - Heavy CPU (multi-pass encoding)
class ProcessPoolDispatcher(Dispatcher):
    def __init__(self, workers: int = 2):
        self.executor = ProcessPoolExecutor(max_workers=workers)
    # (Implementation details...)

# 4. GPUDispatcher - GPU-accelerated (ML inference, shaders)
class GPUDispatcher(Dispatcher):
    def __init__(self, device: str = 'cuda:0'):
        self.device = torch.device(device)

    async def schedule(self, coro):
        await coro  # GPU work happens within coroutine
```

---

## Message Types

**Standard message types for ring buffers.**

```python
@dataclass
class VideoFrame:
    """Video frame message (CPU or GPU)."""
    data: np.ndarray | torch.Tensor  # CPU numpy or GPU tensor
    timestamp: float
    frame_number: int
    width: int
    height: int

    def is_gpu(self) -> bool:
        return isinstance(self.data, torch.Tensor) and self.data.is_cuda

@dataclass
class AudioBuffer:
    """Audio buffer message."""
    data: np.ndarray  # Shape: (samples, channels), dtype: float32
    sample_rate: int
    timestamp: float
    channels: int

@dataclass
class KeyEvent:
    """Keyboard event message."""
    key: int
    timestamp: float
    modifiers: int = 0

@dataclass
class MouseEvent:
    """Mouse event message."""
    x: int
    y: int
    button: int
    timestamp: float
```

---

## Example Usage

### Simple CPU Handler

```python
class BlurFilter(StreamHandler):
    """CPU blur filter (single capability)."""

    def __init__(self):
        super().__init__()
        # CPU-only ports
        self.inputs['video'] = VideoInput('video', capabilities=['cpu'])
        self.outputs['video'] = VideoOutput('video', capabilities=['cpu'])

    async def process(self, tick: TimedTick):
        frame = self.inputs['video'].read_latest()
        if frame:
            blurred = cv2.GaussianBlur(frame.data, (5, 5), 0)  # CPU numpy
            self.outputs['video'].write(VideoFrame(blurred, tick.timestamp, ...))

# Usage
blur = Stream(BlurFilter(), dispatcher='asyncio')
runtime = StreamRuntime(fps=30)
runtime.add_stream(blur)
await runtime.start()
```

### Flexible Handler (CPU or GPU)

```python
class AdaptiveFilter(StreamHandler):
    """Filter that works on CPU or GPU (flexible capabilities)."""

    def __init__(self):
        super().__init__()
        # Flexible ports - can work with either
        self.inputs['video'] = VideoInput('video', capabilities=['cpu', 'gpu'])
        self.outputs['video'] = VideoOutput('video', capabilities=['cpu', 'gpu'])

    async def process(self, tick: TimedTick):
        frame = self.inputs['video'].read_latest()
        if frame:
            # Check negotiated memory space
            if self.inputs['video'].negotiated_memory == 'gpu':
                # GPU path
                result = torch_filter(frame.data)  # GPU tensor operations
            else:
                # CPU path
                result = cv2.GaussianBlur(frame.data, (5, 5), 0)  # numpy

            self.outputs['video'].write(VideoFrame(result, tick.timestamp, ...))

# Runtime negotiates based on connections
adaptive = Stream(AdaptiveFilter(), dispatcher='asyncio')
```

### Mixed CPU/GPU Pipeline with Auto-Transfer

```python
# Create handlers
camera = CameraHandler(device_id=0)
ml_model = MLInferenceHandler(model)  # GPU-only
display = DisplayHandler()

# Create streams
camera_stream = Stream(camera, dispatcher='asyncio')
ml_stream = Stream(ml_model, dispatcher='gpu')
display_stream = Stream(display, dispatcher='asyncio')

# Add to runtime
runtime = StreamRuntime(fps=30)
runtime.add_stream(camera_stream)
runtime.add_stream(ml_stream)
runtime.add_stream(display_stream)

# Connect (runtime auto-inserts transfers)
runtime.connect(camera.outputs['video'], ml_model.inputs['video'])
# ⚠️ WARNING: Auto-inserting cpu→gpu transfer for video (performance cost ~2ms)

runtime.connect(ml_model.outputs['video'], display.inputs['video'])
# ⚠️ WARNING: Auto-inserting gpu→cpu transfer for video (performance cost ~2ms)

await runtime.start()
```

### Explicit Transfer Control

```python
# Create handlers
camera = CameraHandler(device_id=0)
to_gpu = CPUtoGPUTransferHandler(device='cuda:0')
ml_model = MLInferenceHandler(model)
to_cpu = GPUtoCPUTransferHandler()
display = DisplayHandler()

# Create streams with explicit dispatchers
camera_stream = Stream(camera, dispatcher='asyncio')
to_gpu_stream = Stream(to_gpu, dispatcher='gpu')  # Transfer on GPU thread
ml_stream = Stream(ml_model, dispatcher='gpu')
to_cpu_stream = Stream(to_cpu, dispatcher='asyncio')
display_stream = Stream(display, dispatcher='asyncio')

# Add to runtime
runtime = StreamRuntime(fps=30)
for stream in [camera_stream, to_gpu_stream, ml_stream, to_cpu_stream, display_stream]:
    runtime.add_stream(stream)

# Connect explicitly (no auto-transfers)
runtime.connect(camera.outputs['video'], to_gpu.inputs['in'])
runtime.connect(to_gpu.outputs['out'], ml_model.inputs['video'])
runtime.connect(ml_model.outputs['video'], to_cpu.inputs['in'])
runtime.connect(to_cpu.outputs['out'], display.inputs['video'])
# ✅ All connections negotiated successfully (no warnings)

await runtime.start()
```

### Multi-Input Compositor

```python
class VideoCompositor(StreamHandler):
    """Multi-input compositor (CPU-only for simplicity)."""

    def __init__(self):
        super().__init__()
        # Multiple inputs
        self.inputs['background'] = VideoInput('background', capabilities=['cpu'])
        self.inputs['overlay1'] = VideoInput('overlay1', capabilities=['cpu'])
        self.inputs['overlay2'] = VideoInput('overlay2', capabilities=['cpu'])
        self.outputs['video'] = VideoOutput('video', capabilities=['cpu'])

    async def process(self, tick: TimedTick):
        # Clock-synchronized reads
        bg = self.inputs['background'].read_latest()
        ov1 = self.inputs['overlay1'].read_latest()
        ov2 = self.inputs['overlay2'].read_latest()

        if bg:
            result = self.composite(bg, ov1, ov2)
            self.outputs['video'].write(result)

# Build pipeline
camera1 = CameraHandler(device_id=0)
camera2 = CameraHandler(device_id=1)
camera3 = CameraHandler(device_id=2)
compositor = VideoCompositor()
display = DisplayHandler()

# Create streams
streams = [
    Stream(camera1, dispatcher='asyncio'),
    Stream(camera2, dispatcher='asyncio'),
    Stream(camera3, dispatcher='asyncio'),
    Stream(compositor, dispatcher='asyncio'),
    Stream(display, dispatcher='asyncio'),
]

# Add to runtime
runtime = StreamRuntime(fps=60)
for stream in streams:
    runtime.add_stream(stream)

# Connect
runtime.connect(camera1.outputs['video'], compositor.inputs['background'])
runtime.connect(camera2.outputs['video'], compositor.inputs['overlay1'])
runtime.connect(camera3.outputs['video'], compositor.inputs['overlay2'])
runtime.connect(compositor.outputs['video'], display.inputs['video'])

await runtime.start()
```

---

## Flat Handler Registry

**All handlers are siblings in runtime registry.**

No handler spawning in Phase 3. All streams added upfront:

```python
runtime = StreamRuntime(fps=60)
runtime.add_stream(camera_stream)
runtime.add_stream(filter_stream)
runtime.add_stream(display_stream)
runtime.start()  # No new streams after this (Phase 3)
```

**Phase 4+:** May add `runtime.spawn_stream(stream)` for dynamic topology.

**Why flat registry?**
- Maintains handler independence (realtime requirement)
- No parent-child ownership
- Easier supervision and restart
- Simpler lifecycle management

---

## Network-Transparent Design

**Network is just another I/O type - handler concern, not runtime concern.**

Runtime doesn't know about network addressing. Handlers handle their own I/O:

```python
class RTPSendHandler(StreamHandler):
    """Send SMPTE ST 2110 RTP stream."""

    def __init__(self, host: str, port: int):
        super().__init__()
        self.inputs['stream'] = VideoInput('stream', capabilities=['cpu'])
        self.socket = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        self.dest = (host, port)
        self.sequence = 0

    async def process(self, tick):
        frame = self.inputs['stream'].read_latest()
        if frame:
            # Encode to SMPTE 2110 RTP packet
            rtp_packet = encode_smpte_2110_rtp(frame, self.sequence)
            self.socket.sendto(rtp_packet, self.dest)
            self.sequence += 1

# Usage - manual addressing (Phase 3)
rtp_send = RTPSendHandler(host='192.168.1.100', port=5004)
rtp_stream = Stream(rtp_send, dispatcher='asyncio')
runtime.add_stream(rtp_stream)
```

**Network addressing/discovery is Phase 4+ feature.**

---

## SMPTE ST 2110 Alignment

**Professional broadcast standards:**

- **RTP/UDP transport** - SMPTE ST 2110-20/22/30/40
- **PTP timestamps** - IEEE 1588 microsecond accuracy
- **Jitter buffers** - 1-10ms for packet reordering
- **Ring buffers** - Match broadcast triple-buffer practice
- **Port-per-output** - One UDP port per stream output
- **Control/data plane separation** - URIs for control, RTP for data

---

## Performance Considerations

### Zero-Copy Pipeline

**Bad (copies data):**
```python
def process(self, tick):
    frame = self.inputs['video'].read_latest().copy()  # ❌ Copy
    result = transform(frame.copy())  # ❌ Another copy
    self.outputs['video'].write(result.copy())  # ❌ Another copy
```

**Good (zero-copy):**
```python
async def process(self, tick):
    frame = self.inputs['video'].read_latest()  # Reference only
    if frame:
        result = transform(frame)  # In-place or new allocation
        self.outputs['video'].write(result)  # Reference only
```

### GPU Efficiency

**Keep data on GPU (explicit transfers):**
```python
# Pipeline: CPU → GPU → GPU → GPU → CPU
camera >> to_gpu >> detector >> overlay >> to_cpu >> display
# Only 2 transfers (boundaries), GPU work stays on GPU
```

**Ring buffers hold GPU tensor references:**
```python
ring_buffer = RingBuffer[torch.Tensor]()
gpu_tensor = torch.zeros((1920, 1080, 3), device='cuda')
ring_buffer.write(gpu_tensor)  # Just stores reference
frame = ring_buffer.read_latest()  # Returns reference, stays on GPU
```

### Realtime Guarantees

**Frame dropping (not queueing):**
```python
# Tick 1: Write frame_A
# Tick 2: Write frame_B (overwrites oldest)
# Tick 3: Write frame_C
# Tick 4: Handler finally reads → Gets frame_C (latest)
#         Frames A and B automatically dropped
```

**Performance targets:**
- 1080p60: < 16ms per frame (P99)
- Jitter: < 1ms (P99 - P50)
- CPU: < 5% per handler
- Memory: Fixed (ring buffers pre-allocated)

---

## Benefits

1. **Composable** - Like Unix pipes for streams
2. **Zero-copy** - References flow, not data copies
3. **GPU-efficient** - Data stays on GPU with explicit transfers
4. **Realtime** - Clock-driven, automatic frame dropping
5. **Professional** - SMPTE/PTP/genlock support
6. **Concurrent** - Handlers run independently
7. **Flexible** - Capability negotiation handles CPU/GPU
8. **Type-safe** - Runtime checks port type + memory space compatibility
9. **SDK-appropriate** - Explicit dispatchers, visible costs
10. **GStreamer-proven** - Capability-based negotiation works

---

## Philosophy

**Core insight:** Handlers are pure processing logic. Runtime provides execution context and negotiates capabilities.

This separation enables:
- Handler reusability across contexts
- Simple, predictable API
- Easy testing (handlers are just functions)
- Runtime controls everything (lifecycle, timing, execution, negotiation)

**Inspired by:**
- **Cloudflare Actors** - Runtime manages lifecycle
- **GStreamer** - Capability-based negotiation
- **Unix pipes** - Composable primitives
- **SMPTE ST 2110** - Professional broadcast standards

---

## References

- Actor Model: https://en.wikipedia.org/wiki/Actor_model
- SMPTE ST 2110: https://www.smpte.org/
- PTP (IEEE 1588): https://en.wikipedia.org/wiki/Precision_Time_Protocol
- GStreamer Capabilities: https://gstreamer.freedesktop.org/documentation/plugin-development/advanced/negotiation.html
- Cloudflare Durable Objects: https://developers.cloudflare.com/durable-objects/

---

# Addendum: GStreamer Interoperability

**Note:** This section describes a future addon package (`streamlib-gstreamer`) that is NOT part of core streamlib. This would be an optional integration for users who want to leverage GStreamer's 100+ plugins within streamlib pipelines.

## Why GStreamer Interop Works Naturally

Our capability-based port design was inspired by GStreamer, making interop a natural fit:

| GStreamer | streamlib |
|-----------|-----------|
| `memory:SystemMemory` caps | `capabilities=['cpu']` |
| `memory:GLMemory` caps | `capabilities=['gpu']` |
| Element pads | StreamInput/StreamOutput ports |
| Caps negotiation | Runtime capability negotiation |
| GstBuffer | VideoFrame with numpy/torch |

## Three Interop Approaches

### Approach A: Wrap Individual GStreamer Elements

Wrap any GStreamer element as a StreamHandler:

```python
from streamlib.addons.gstreamer import GstElementHandler
from gi.repository import Gst
import numpy as np

class GstElementHandler(StreamHandler):
    """Wrap any GStreamer element as StreamHandler."""

    def __init__(self, element_name: str, **properties):
        super().__init__()

        # Create GStreamer element
        self.gst_element = Gst.ElementFactory.make(element_name, None)

        # Set properties
        for key, value in properties.items():
            self.gst_element.set_property(key, value)

        # Map GStreamer caps to our capabilities
        # GstCaps with memory:SystemMemory → ['cpu']
        # GstCaps with memory:GLMemory → ['gpu']
        src_caps = self._parse_gst_caps('src')
        self.outputs['src'] = VideoOutput('src', capabilities=src_caps)

        sink_caps = self._parse_gst_caps('sink')
        if sink_caps:
            self.inputs['sink'] = VideoInput('sink', capabilities=sink_caps)

        # Setup appsrc/appsink bridge
        self.appsrc = Gst.ElementFactory.make('appsrc', None)
        self.appsink = Gst.ElementFactory.make('appsink', None)

        # Mini pipeline: appsrc → element → appsink
        self.pipeline = Gst.Pipeline.new(None)
        self.pipeline.add(self.appsrc)
        self.pipeline.add(self.gst_element)
        self.pipeline.add(self.appsink)

        self.appsrc.link(self.gst_element)
        self.gst_element.link(self.appsink)

        self.pipeline.set_state(Gst.State.PLAYING)

    def _parse_gst_caps(self, pad_name):
        """Map GStreamer caps to streamlib capabilities."""
        pad = self.gst_element.get_static_pad(pad_name)
        if not pad:
            return None

        caps = pad.query_caps(None)

        # Check for GPU memory in caps
        for i in range(caps.get_size()):
            features = caps.get_features(i)

            if features and features.contains('memory:GLMemory'):
                return ['gpu']
            elif features and features.contains('memory:SystemMemory'):
                return ['cpu']

        return ['cpu']  # Default to CPU

    async def process(self, tick: TimedTick):
        # Read from streamlib ring buffer
        frame = self.inputs['sink'].read_latest()
        if frame:
            # Convert numpy/torch to GstBuffer
            gst_buffer = self._to_gst_buffer(frame.data)
            self.appsrc.emit('push-buffer', gst_buffer)

        # Pull from GStreamer appsink
        sample = self.appsink.emit('try-pull-sample', 0)
        if sample:
            gst_buffer = sample.get_buffer()
            output_data = self._from_gst_buffer(gst_buffer)

            # Write to streamlib ring buffer
            self.outputs['src'].write(VideoFrame(
                output_data,
                tick.timestamp,
                tick.frame_number,
                frame.width,
                frame.height
            ))

    def _to_gst_buffer(self, array):
        """Convert numpy/torch to GstBuffer (zero-copy when possible)."""
        if isinstance(array, torch.Tensor):
            array = array.cpu().numpy()

        gst_buffer = Gst.Buffer.new_allocate(None, array.nbytes, None)
        gst_buffer.fill(0, array.tobytes())
        return gst_buffer

    def _from_gst_buffer(self, gst_buffer):
        """Convert GstBuffer to numpy (zero-copy with memoryview)."""
        success, map_info = gst_buffer.map(Gst.MapFlags.READ)
        if not success:
            return None

        # Zero-copy via memoryview
        array = np.frombuffer(map_info.data, dtype=np.uint8)
        gst_buffer.unmap(map_info)

        # TODO: Parse caps to get actual dimensions
        return array.reshape((1080, 1920, 3))


# Usage - wrap videotestsrc
videotestsrc = GstElementHandler('videotestsrc', pattern='smpte')
videotestsrc_stream = Stream(videotestsrc, dispatcher='asyncio')

# Mix with pure streamlib handlers
blur = BlurFilter()  # Pure streamlib handler
blur_stream = Stream(blur, dispatcher='asyncio')

runtime = StreamRuntime(fps=30)
runtime.add_stream(videotestsrc_stream)
runtime.add_stream(blur_stream)

# Connect GStreamer element to streamlib handler
runtime.connect(videotestsrc.outputs['src'], blur.inputs['video'])
# ✅ Capabilities negotiated automatically!

await runtime.start()
```

### Approach B: Wrap Entire GStreamer Pipeline

Wrap a complete GStreamer pipeline as a single handler:

```python
from streamlib.addons.gstreamer import GstPipelineHandler

class GstPipelineHandler(StreamHandler):
    """Wrap entire GStreamer pipeline as single handler."""

    def __init__(self, pipeline_str: str):
        super().__init__()

        # Parse GStreamer pipeline string
        # e.g., "videotestsrc ! videoconvert ! videoscale"
        self.pipeline = Gst.parse_launch(pipeline_str + " ! appsink name=sink")

        self.appsink = self.pipeline.get_by_name('sink')
        self.outputs['video'] = VideoOutput('video', capabilities=['cpu'])

        self.pipeline.set_state(Gst.State.PLAYING)

    async def process(self, tick: TimedTick):
        # Pull frame from GStreamer pipeline
        sample = self.appsink.emit('try-pull-sample', 0)
        if sample:
            gst_buffer = sample.get_buffer()
            array = self._from_gst_buffer(gst_buffer)

            self.outputs['video'].write(VideoFrame(
                array, tick.timestamp, tick.frame_number, 1920, 1080
            ))

    def _from_gst_buffer(self, gst_buffer):
        success, map_info = gst_buffer.map(Gst.MapFlags.READ)
        if not success:
            return None

        array = np.frombuffer(map_info.data, dtype=np.uint8)
        gst_buffer.unmap(map_info)
        return array.reshape((1080, 1920, 3))


# Usage - entire pipeline as handler
gst_pipeline = GstPipelineHandler(
    "videotestsrc pattern=smpte ! videoconvert ! videoscale width=1920 height=1080"
)
gst_stream = Stream(gst_pipeline, dispatcher='asyncio')

# Connect to streamlib display
display = DisplayHandler()
display_stream = Stream(display, dispatcher='asyncio')

runtime = StreamRuntime(fps=30)
runtime.add_stream(gst_stream)
runtime.add_stream(display_stream)
runtime.connect(gst_pipeline.outputs['video'], display.inputs['video'])

await runtime.start()
```

### Approach C: Bidirectional (Input + Output)

Use GStreamer as a filter in the middle of a streamlib pipeline:

```python
from streamlib.addons.gstreamer import GstBidirectionalHandler

class GstBidirectionalHandler(StreamHandler):
    """GStreamer pipeline with both input and output."""

    def __init__(self, pipeline_str: str):
        super().__init__()

        # Parse pipeline with named appsrc/appsink
        # e.g., "appsrc name=src ! videoconvert ! videoscale ! appsink name=sink"
        self.pipeline = Gst.parse_launch(pipeline_str)

        self.appsrc = self.pipeline.get_by_name('src')
        self.appsink = self.pipeline.get_by_name('sink')

        # Detect capabilities from pipeline elements
        self.inputs['video'] = VideoInput('video', capabilities=['cpu'])
        self.outputs['video'] = VideoOutput('video', capabilities=['cpu'])

        self.pipeline.set_state(Gst.State.PLAYING)

    async def process(self, tick: TimedTick):
        # Pull from streamlib, push to GStreamer
        frame = self.inputs['video'].read_latest()
        if frame:
            gst_buffer = self._to_gst_buffer(frame.data)
            self.appsrc.emit('push-buffer', gst_buffer)

        # Pull from GStreamer, push to streamlib
        sample = self.appsink.emit('try-pull-sample', 0)
        if sample:
            gst_buffer = sample.get_buffer()
            array = self._from_gst_buffer(gst_buffer)
            self.outputs['video'].write(VideoFrame(
                array, tick.timestamp, tick.frame_number, 1920, 1080
            ))

    def _to_gst_buffer(self, array):
        if isinstance(array, torch.Tensor):
            array = array.cpu().numpy()
        gst_buffer = Gst.Buffer.new_allocate(None, array.nbytes, None)
        gst_buffer.fill(0, array.tobytes())
        return gst_buffer

    def _from_gst_buffer(self, gst_buffer):
        success, map_info = gst_buffer.map(Gst.MapFlags.READ)
        if not success:
            return None
        array = np.frombuffer(map_info.data, dtype=np.uint8)
        gst_buffer.unmap(map_info)
        return array.reshape((1080, 1920, 3))


# Usage - GStreamer as filter in streamlib pipeline
camera = CameraHandler(device_id=0)
gst_filter = GstBidirectionalHandler(
    "appsrc name=src ! videoconvert ! videoscale width=640 height=480 ! appsink name=sink"
)
display = DisplayHandler()

# Create streams
camera_stream = Stream(camera, dispatcher='asyncio')
filter_stream = Stream(gst_filter, dispatcher='asyncio')
display_stream = Stream(display, dispatcher='asyncio')

# Build pipeline
runtime = StreamRuntime(fps=30)
runtime.add_stream(camera_stream)
runtime.add_stream(filter_stream)
runtime.add_stream(display_stream)

# Wire: camera → GStreamer filter → display
runtime.connect(camera.outputs['video'], gst_filter.inputs['video'])
runtime.connect(gst_filter.outputs['video'], display.inputs['video'])

await runtime.start()
```

## Memory Sharing Optimization

**CPU memory (zero-copy):**
```python
# numpy array → GstBuffer via memoryview (zero-copy)
mv = memoryview(numpy_array)
gst_buffer = Gst.Buffer.new_wrapped(mv)

# GstBuffer → numpy array via memoryview (zero-copy)
success, map_info = gst_buffer.map(Gst.MapFlags.READ)
numpy_array = np.frombuffer(map_info.data, dtype=np.uint8)
```

**GPU memory (zero-copy with GstGLMemory):**
```python
# torch tensor → GstGLMemory (share OpenGL texture)
gl_context = GstGL.Context.new()
gl_memory = GstGL.GLMemory.wrapped(
    gl_context,
    torch_tensor.data_ptr(),  # OpenGL texture ID
    torch_tensor.numel()
)
gst_buffer = Gst.Buffer.new()
gst_buffer.append_memory(gl_memory)

# Both GStreamer and PyTorch point to same GPU memory
```

## Clock Synchronization

**Option 1: Use streamlib clock to drive GStreamer:**
```python
class GstElementHandler(StreamHandler):
    async def process(self, tick: TimedTick):
        # Set GstBuffer timestamp from streamlib tick
        gst_buffer.pts = int(tick.timestamp * Gst.SECOND)
        gst_buffer.dts = gst_buffer.pts
```

**Option 2: Use GStreamer clock as streamlib Clock:**
```python
from streamlib.addons.gstreamer import GstClockAdapter

class GstClockAdapter(Clock):
    """Wrap GstClock as streamlib Clock."""

    def __init__(self, gst_clock: Gst.Clock, fps: float = 60.0):
        self.gst_clock = gst_clock
        self.fps = fps
        self.period = 1.0 / fps
        self.frame_number = 0

    async def next_tick(self) -> TimedTick:
        # Wait until next frame time
        gst_time = self.gst_clock.get_time()
        target_time = (self.frame_number + 1) * self.period * Gst.SECOND

        if gst_time < target_time:
            wait_time = (target_time - gst_time) / Gst.SECOND
            await asyncio.sleep(wait_time)

        tick = TimedTick(
            timestamp=self.gst_clock.get_time() / Gst.SECOND,
            frame_number=self.frame_number,
            clock_source_id='gstreamer',
            fps=self.fps
        )
        self.frame_number += 1
        return tick

# Use GStreamer's clock for entire runtime
gst_clock = Gst.SystemClock.obtain()
runtime = StreamRuntime(clock=GstClockAdapter(gst_clock, fps=30))
```

## Benefits of GStreamer Interop

1. **Access 100+ GStreamer plugins** - codecs, filters, sources, sinks
2. **Hardware acceleration** - VA-API, NVENC, QuickSync via GStreamer
3. **Mature codecs** - H.264/H.265/VP9/AV1 encoding/decoding
4. **RTP/RTSP support** - GStreamer's battle-tested network stack
5. **Mix and match** - Use GStreamer plugins where needed, streamlib elsewhere
6. **Easy migration** - Existing GStreamer pipelines can be wrapped
7. **Type-safe** - Capability negotiation catches CPU/GPU mismatches

## Use Cases

**Use streamlib handlers when:**
- Writing custom processing logic in Python
- Integrating ML models (PyTorch, ONNX)
- Need explicit control over memory transfers
- Building custom compositors/effects
- Prototyping new algorithms

**Use GStreamer elements when:**
- Need hardware codec acceleration
- Using proven plugins (x264enc, nvh264enc)
- RTP/RTSP networking
- File format support (MP4, MKV, WebM)
- Camera capture with V4L2

**Mix both when:**
- GStreamer for I/O, streamlib for processing
- Custom ML in streamlib, encoding in GStreamer
- Rapid prototyping with GStreamer, optimize with streamlib

## Package Structure

```
streamlib-gstreamer/
├── pyproject.toml
├── README.md
└── src/
    └── streamlib_gstreamer/
        ├── __init__.py
        ├── element_handler.py      # GstElementHandler
        ├── pipeline_handler.py     # GstPipelineHandler
        ├── bidirectional.py        # GstBidirectionalHandler
        ├── clock.py                # GstClockAdapter
        └── utils.py                # Caps parsing, buffer conversion
```

**Dependencies:**
```toml
[project]
name = "streamlib-gstreamer"
dependencies = [
    "streamlib>=0.1.0",
    "PyGObject>=3.42.0",  # GStreamer Python bindings
]
```

**Installation:**
```bash
# Core streamlib (no GStreamer)
pip install streamlib

# With GStreamer addon
pip install streamlib-gstreamer
```

This addon would unlock GStreamer's ecosystem while maintaining streamlib's clean SDK design!

---

## Performance & Implementation Notes

### Display Sink Implementation (OpenCV)

When implementing DisplaySink with OpenCV on macOS, these fixes are required:

**Required for macOS compatibility:**
```python
import cv2

# Call once at module import (not per-instance)
cv2.startWindowThread()

class DisplaySink(StreamHandler):
    def __init__(self, window_name: str = "streamlib"):
        super().__init__()
        self.window_name = window_name
        self.inputs['video'] = VideoInput('video', capabilities=['cpu'])

        # Create window
        cv2.namedWindow(self.window_name, cv2.WINDOW_AUTOSIZE)  # Not WINDOW_NORMAL

        # Bring to foreground on macOS
        cv2.setWindowProperty(
            self.window_name,
            cv2.WND_PROP_TOPMOST,
            1
        )

    async def process(self, tick: TimedTick):
        frame = self.inputs['video'].read_latest()
        if frame:
            # Convert RGB → BGR for OpenCV
            bgr = cv2.cvtColor(frame.data, cv2.COLOR_RGB2BGR)
            cv2.imshow(self.window_name, bgr)

            # Longer delay for better event processing
            cv2.waitKey(10)  # Not 1ms
```

**Key fixes:**
- `cv2.startWindowThread()` - Required for macOS window event loop
- `cv2.WINDOW_AUTOSIZE` - Better cross-platform compatibility than `WINDOW_NORMAL`
- `cv2.setWindowProperty(WND_PROP_TOPMOST)` - Brings window to foreground
- `cv2.waitKey(10)` - Longer delay (10ms) for reliable event processing

### Current Performance Constraints

**Python implementation limitations:**

| Resolution | FPS | Status |
|------------|-----|--------|
| 640×480 | 30 | ✅ Works reliably |
| 1280×720 | 30 | ⚠️ May drop frames |
| 1920×1080 | 30 | ❌ Performance issues |
| 640×480 | 60 | ⚠️ Borderline |
| 1920×1080 | 60 | ❌ Too slow |

**Bottlenecks identified:**
1. **Compositor alpha blending** - NumPy operations become expensive at high resolutions
2. **Event loop contention** - AsyncIO struggles with high-frequency ticks
3. **Frame copying** - Python overhead for buffer management

**For prototyping and development:**
- Use **640×480 @ 30 FPS** as baseline
- Test at target resolution/FPS early to identify bottlenecks
- Profile before optimizing (don't assume what's slow)

### Future Optimization Paths

**When you need higher performance:**

1. **Optimize compositor with Numba JIT:**
   ```python
   import numba

   @numba.jit(nopython=True, parallel=True)
   def blend_layers_fast(base, overlay, alpha):
       # Vectorized operations compiled to machine code
       ...
   ```

2. **GPU acceleration (CUDA/OpenCL):**
   ```python
   import torch

   class GPUCompositor(StreamHandler):
       def __init__(self):
           self.inputs['layer0'] = VideoInput('layer0', capabilities=['gpu'])
           self.outputs['video'] = VideoOutput('video', capabilities=['gpu'])

       async def process(self, tick):
           # All operations stay on GPU
           layers = [self.inputs[f'layer{i}'].read_latest() for i in range(4)]
           blended = torch_alpha_blend(layers)  # GPU kernel
           self.outputs['video'].write(blended)
   ```

3. **Move heavy work to thread pool:**
   ```python
   runtime.add_stream(Stream(
       CompositorHandler(...),
       dispatcher='threadpool'  # Not asyncio
   ))
   ```

4. **Consider Rust/C++ for critical paths:**
   - Write Python extension modules for hot loops
   - Keep Python for orchestration, Rust for processing
   - Use PyO3 for seamless Python↔Rust integration

5. **Use GStreamer for hardware acceleration:**
   ```python
   # Let GStreamer handle codec acceleration
   encoder = GstElementHandler('nvh264enc')  # NVIDIA GPU encoder
   ```

**Performance targets (Phase 4):**
- 1080p60: < 16ms per frame
- Jitter: < 1ms
- Zero frame drops under normal load

**Profile first, optimize second** - Don't prematurely optimize based on assumptions. The Python implementation may be sufficient for many use cases, especially with selective GPU acceleration.
