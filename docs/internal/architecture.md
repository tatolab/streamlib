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

**GPU-first, zero-copy architecture:**
- Ring buffers hold references, not data
- GPU textures/buffers stay on GPU throughout pipeline (default)
- CPU operations only when explicitly required
- Runtime automatically handles memory management

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
StreamRuntime (lifecycle, clock, automatic execution, supervision)
    ↓
    Manages multiple Streams
    ↓
Stream (config: handler + optional transport)
    ↓
    Wraps StreamHandler
    ↓
StreamHandler (processing logic)
    ↓
    inputs/outputs → GPU-First Ports → RingBuffers (zero-copy references)
```

**StreamRuntime** = Cloudflare Wrangler (manages lifecycle + automatic optimization)
**Stream** = Configuration wrapper (handler + optional transport)
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
- **GPU-first by default** - all operations use GPU unless configured otherwise
- **Zero-copy** ring buffer reads/writes
- **Clock-driven** via `process(tick)` calls
- **Automatic execution** - runtime infers optimal dispatcher

---

## GPU-First Ports (Simplified)

**Ports are GPU by default. Runtime handles everything automatically.**

```python
class StreamOutput:
    """
    Output port for sending data (GPU by default).
    """

    def __init__(
        self,
        name: str,
        port_type: str,  # 'video', 'audio', 'data'
        allow_cpu: bool = False,  # Optional CPU fallback
        cpu_only: bool = False,   # Force CPU (rare)
        slots: int = 3
    ):
        self.name = name
        self.port_type = port_type
        self.allow_cpu = allow_cpu
        self.cpu_only = cpu_only
        self.buffer = RingBuffer(slots=slots)

    def write(self, data) -> None:
        """Write reference to ring buffer (zero-copy)."""
        self.buffer.write(data)


class StreamInput:
    """
    Input port for receiving data (GPU by default).
    """

    def __init__(
        self,
        name: str,
        port_type: str,  # 'video', 'audio', 'data'
        allow_cpu: bool = False,  # Optional CPU fallback
        cpu_only: bool = False    # Force CPU (rare)
    ):
        self.name = name
        self.port_type = port_type
        self.allow_cpu = allow_cpu
        self.cpu_only = cpu_only
        self.buffer: Optional[RingBuffer] = None

    def connect(self, buffer: RingBuffer) -> None:
        """Connect to ring buffer."""
        self.buffer = buffer

    def read_latest(self):
        """Read latest reference (zero-copy)."""
        if self.buffer is None:
            return None
        return self.buffer.read_latest()


# Convenience factory functions (GPU by default)
def VideoInput(name: str, allow_cpu: bool = False, cpu_only: bool = False) -> StreamInput:
    """Create video input port (GPU by default)."""
    return StreamInput(name, port_type='video', allow_cpu=allow_cpu, cpu_only=cpu_only)

def VideoOutput(name: str, allow_cpu: bool = False, cpu_only: bool = False, slots: int = 3) -> StreamOutput:
    """Create video output port (GPU by default)."""
    return StreamOutput(name, port_type='video', allow_cpu=allow_cpu, cpu_only=cpu_only, slots=slots)

def AudioInput(name: str, allow_cpu: bool = False, cpu_only: bool = False) -> StreamInput:
    """Create audio input port (GPU by default)."""
    return StreamInput(name, port_type='audio', allow_cpu=allow_cpu, cpu_only=cpu_only)

def AudioOutput(name: str, allow_cpu: bool = False, cpu_only: bool = False, slots: int = 3) -> StreamOutput:
    """Create audio output port (GPU by default)."""
    return StreamOutput(name, port_type='audio', allow_cpu=allow_cpu, cpu_only=cpu_only, slots=slots)
```

---

## Automatic Memory Management (Runtime)

**Runtime automatically handles GPU-first memory management. No negotiation needed in typical case.**

```python
class StreamRuntime:
    def connect(
        self,
        output_port: StreamOutput,
        input_port: StreamInput,
        auto_transfer: bool = True
    ) -> None:
        """
        Connect output to input (GPU-first by default).

        Connection rules:
        1. Port types must match (video→video, audio→audio)
        2. GPU by default - just connect the buffers
        3. If CPU fallback needed, auto-insert transfer (rare)
        4. Runtime handles all memory management automatically
        """

        # Check port type compatibility
        if output_port.port_type != input_port.port_type:
            raise TypeError(
                f"Cannot connect {output_port.port_type} output to "
                f"{input_port.port_type} input"
            )

        # GPU-first: most connections just work
        if not output_port.cpu_only and not input_port.cpu_only:
            # Both ports are GPU (default) - direct connection
            input_port.connect(output_port.buffer)
            print(f"✅ Connected {output_port.name} → {input_port.name} (GPU)")
            return

        # Rare case: CPU involved
        if output_port.cpu_only and input_port.cpu_only:
            # Both CPU - direct connection
            input_port.connect(output_port.buffer)
            print(f"✅ Connected {output_port.name} → {input_port.name} (CPU)")
            return

        # Very rare: GPU↔CPU transfer needed
        if not auto_transfer:
            raise TypeError(
                f"Memory space mismatch: output is "
                f"{'CPU' if output_port.cpu_only else 'GPU'}, input is "
                f"{'CPU' if input_port.cpu_only else 'GPU'}. "
                f"Set auto_transfer=True to allow automatic transfer."
            )

        # Auto-insert transfer handler
        self._insert_transfer_handler(output_port, input_port)

    def _insert_transfer_handler(
        self,
        output_port: StreamOutput,
        input_port: StreamInput
    ) -> None:
        """Auto-insert transfer handler (rare case)."""

        if output_port.cpu_only:
            # CPU → GPU transfer
            print(
                f"⚠️  WARNING: Auto-inserting CPU→GPU transfer "
                f"for {output_port.port_type} (performance cost ~2ms). "
                f"Consider making entire pipeline GPU-first."
            )
            transfer = CPUtoWebGPUTransferHandler()
        else:
            # GPU → CPU transfer
            print(
                f"⚠️  WARNING: Auto-inserting GPU→CPU transfer "
                f"for {output_port.port_type} (performance cost ~2ms). "
                f"Consider making entire pipeline GPU-first."
            )
            transfer = WebGPUtoCPUTransferHandler()

        # Add transfer handler to runtime (dispatcher inferred automatically)
        transfer_stream = Stream(transfer)
        self.add_stream(transfer_stream)

        # Wire: output → transfer → input
        transfer.inputs['in'].connect(output_port.buffer)
        input_port.connect(transfer.outputs['out'].buffer)

        self._transfer_handlers.append(transfer)
```

---

## Transfer Handlers (Rare Case)

**Runtime auto-inserts transfer handlers when CPU↔GPU memory spaces don't match.**

Transfer handlers bridge memory spaces:
- `CPUtoWebGPUTransferHandler`: Uploads CPU numpy arrays to WebGPU textures
- `WebGPUtoCPUTransferHandler`: Downloads WebGPU textures to CPU numpy arrays

**Design:**
- Input port: `cpu_only=True` or default GPU
- Output port: opposite memory space
- Uses WebGPU buffer/texture copy operations
- Preserves frame metadata and timing

**Performance cost:** ~2ms per transfer at 1080p. Runtime warns when auto-inserting transfers to encourage GPU-first design.

---

## Stream (Configuration Wrapper)

**Wraps handler with optional transport. Runtime automatically infers execution context.**

```python
class Stream:
    """
    Configuration wrapper for StreamHandler.

    Stream = Handler + Transport (optional)

    Runtime automatically infers optimal execution context based on handler operations.
    No explicit dispatcher needed - the system is GPU-first and automatic.

    Transport is optional (only for I/O handlers):
    - Internal processing handlers don't need transport
    - I/O handlers (camera, display, network) use transport for metadata
    """

    def __init__(
        self,
        handler: StreamHandler,
        transport: Optional[Dict] = None,  # Optional, for I/O handlers only

        # Lifecycle policies (Phase 4)
        restart_policy: str = 'never',  # 'never', 'on-failure', 'always'
        concurrency_limit: Optional[int] = None,
        time_limit: Optional[int] = None,
        **kwargs
    ):
        self.handler = handler
        self.transport = transport
        self.config = {
            'transport': transport,
            'restart_policy': restart_policy,
            'concurrency_limit': concurrency_limit,
            'time_limit': time_limit,
            **kwargs
        }
```

**Transport usage:**
- Internal processing handlers: No transport needed
- I/O handlers (camera, display): Transport provides metadata for device registry
- Network handlers (RTP, WebSocket): Transport provides connection metadata
- Phase 4: Network discovery and addressing

---

## StreamRuntime (Lifecycle Manager)

**Central runtime that manages all handlers with automatic execution inference.**

```python
class StreamRuntime:
    """
    Runtime for managing StreamHandler lifecycle.

    Inspired by Cloudflare Wrangler + automatic optimization.

    Responsibilities:
    - Provide shared clock for all handlers
    - Automatically infer optimal execution context for each handler
    - Start/stop handlers
    - Insert transfer handlers when needed (rare)
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

        # GPU context (WebGPU)
        self.gpu_context = self._create_gpu_context()

        # Track auto-inserted transfer handlers
        self._transfer_handlers: List[StreamHandler] = []

    def add_stream(self, stream: Stream) -> None:
        """
        Add stream to runtime.

        Handler remains inert until runtime.start().
        Runtime automatically infers optimal execution context.
        """
        handler = stream.handler
        handler._runtime = self
        handler._clock = self.clock

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
                # Process handler (execution context inferred automatically)
                await handler.process(tick)
            except Exception as e:
                # Phase 4: Handle restart policy
                # For now: crash runtime
                raise
```

---

## Ring Buffers (Zero-Copy)

**Fixed-size circular buffers with latest-read semantics.**

### Core Design

**RingBuffer[T]**: Generic circular buffer
- **Fixed size**: 3 slots by default (broadcast standard)
- **Latest-read semantics**: Always get most recent data, old data automatically dropped
- **Zero-copy**: Stores references only, not data copies
- **Thread-safe**: Uses locks for concurrent access
- **Operations**: `write(data)` stores reference, `read_latest()` returns latest reference

**Why 3 slots?**
- Matches professional broadcast practice (triple buffering)
- One for writer, one for reader, one spare
- Minimal latency with minimal memory overhead

### GPU Ring Buffers

**WebGPURingBuffer**: Pre-allocated GPU texture ring buffer
- **Pre-allocation**: Textures created at initialization to avoid runtime allocation overhead
- **Zero-copy**: Returns texture references, not copies
- **Operations**: `get_write_texture()`, `advance()`, `get_read_texture()`
- **Use case**: High-performance GPU pipelines where allocation overhead is unacceptable

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

## Message Types

**Standard message types for ring buffers.**

```python
@dataclass
class VideoFrame:
    """Video frame message (CPU or GPU)."""
    data: np.ndarray | wgpu.GPUTexture  # CPU numpy or GPU texture
    timestamp: float
    frame_number: int
    width: int
    height: int
    metadata: Dict[str, Any] = None  # {'memory': 'cpu'/'webgpu', 'format': '...'}

    def is_gpu(self) -> bool:
        return isinstance(self.data, wgpu.GPUTexture)

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

## Usage Patterns

**For complete examples, see:**
- `examples/` directory: Working Python implementations
- `docs/guides/quickstart.md`: Basic usage tutorial
- `docs/guides/composition.md`: Pipeline composition patterns
- `docs/api/` directory: API reference with examples

**Common patterns:**
1. **All-GPU pipeline** (recommended): All handlers use default GPU ports, runtime keeps data on GPU throughout
2. **CPU-only handler** (rare): Use `cpu_only=True` for legacy compatibility
3. **Mixed with auto-transfer** (rare): Runtime auto-inserts CPU↔GPU transfers when needed
4. **Multi-input handlers**: Handlers can have multiple inputs (e.g., compositor with 3+ video inputs)

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

### Design Principle

Runtime doesn't manage network addressing. Network handlers are regular handlers that:
- Own their sockets/connections
- Handle protocol encoding/decoding
- Manage their own I/O in `process()` method
- Use standard input/output ports

### Network Handler Pattern

Network handlers (RTP, WebRTC, WebSocket) follow the same pattern as other I/O handlers:
- Initialize connections in `on_start()`
- Read from input ports, send over network in `process()`
- Receive from network, write to output ports in `process()`
- Clean up connections in `on_stop()`

**Phase 3:** Manual addressing (host/port in constructor)
**Phase 4+:** Network discovery and automatic addressing

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

**Principle:** Ring buffers store references, not copies
- Port operations (`read_latest()`, `write()`) pass references
- Avoid explicit `.copy()` calls on frame data
- GPU textures stay on GPU, CPU arrays stay in place
- Transform operations should work in-place or create new allocations as needed

### GPU Efficiency

**Principle:** Data stays on GPU by default
- All handlers use GPU ports by default
- Runtime automatically maintains GPU-resident data
- Ring buffers hold GPU texture references
- CPU↔GPU transfers only when explicitly needed (rare)
- Runtime warns when inserting transfers to encourage optimization

### Realtime Guarantees

**Principle:** Latest-read semantics, not queueing
- Ring buffers always return most recent data
- Old frames automatically dropped if reader is slow
- No backpressure, no unbounded queues
- Predictable, deterministic latency
- Ring buffers are pre-allocated (fixed memory usage)

**Performance targets:**
- 1080p60: < 16ms per frame (P99)
- Jitter: < 1ms (P99 - P50)
- CPU: < 5% per handler
- Memory: Fixed (pre-allocated ring buffers)

---

## Benefits

1. **Composable** - Like Unix pipes for streams
2. **Zero-copy** - References flow, not data copies
3. **GPU-first** - Data stays on GPU automatically, no explicit management
4. **Realtime** - Clock-driven, automatic frame dropping
5. **Professional** - SMPTE/PTP/genlock support
6. **Concurrent** - Handlers run independently
7. **Simple** - GPU by default, CPU only when needed
8. **Type-safe** - Runtime checks port type compatibility
9. **Automatic** - Runtime infers optimal execution context
10. **Cross-platform** - WebGPU works everywhere (Metal/D3D12/Vulkan)

---

## Philosophy

**Core insight:** Handlers are pure processing logic. Runtime provides GPU-first execution automatically.

This separation enables:
- Handler reusability across contexts
- Simple, predictable API (GPU by default)
- Easy testing (handlers are just functions)
- Runtime controls everything (lifecycle, timing, automatic execution)

**Inspired by:**
- **Cloudflare Actors** - Runtime manages lifecycle
- **WebGPU** - Cross-platform GPU-first design
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

## WebGPU GPU-First Architecture

### Why WebGPU

streamlib uses **WebGPU** as the unified GPU backend for all platforms. This is a deliberate architectural choice that differentiates streamlib from GStreamer and other streaming frameworks.

**WebGPU advantages:**

1. **Cross-platform without platform-specific code:**
   - macOS: Uses Metal backend
   - Windows: Uses DirectX 12 backend
   - Linux: Uses Vulkan backend
   - Web: Native browser WebGPU

2. **AI agent friendly:**
   - Simple, modern API (much simpler than Vulkan)
   - Clear resource model (textures, buffers, pipelines)
   - Explicit command encoding
   - No complex state management

3. **Unified compute and rendering:**
   - Compute shaders for video effects, audio FFT, ML
   - Render pipelines for compositing, YUV→RGB conversion
   - Single API for both use cases

4. **Industry standard:**
   - Supported by Chrome, Firefox, Safari
   - Growing ecosystem and tooling
   - Future-proof for web deployment

5. **Excellent Python bindings:**
   - `wgpu-py` provides full WebGPU API in Python
   - Pythonic interface with good documentation

6. **Native Skia integration:**
   - Skia has WebGPU backend
   - AI-generated drawing handlers work seamlessly

7. **ML framework support:**
   - ONNX Runtime has WebGPU execution provider
   - TensorFlow.js uses WebGPU
   - Growing ML ecosystem

**Why not Vulkan?**
- Much more complex API (1000+ page spec)
- Requires significant expertise to use correctly
- Harder for AI agents to generate correct code
- No unified rendering + compute (need separate extensions)
- Platform-specific code still required (Windows/Linux differences)

**Why not Metal?**
- macOS only
- Can't support Windows/Linux
- Would require platform-specific codepaths

### WebGPU Context

**StreamRuntime creates shared WebGPU context:**

```python
class StreamRuntime:
    def _create_gpu_context(self):
        """Create WebGPU context (auto-detects best backend)."""
        import wgpu

        # Request adapter (auto-selects: Metal/D3D12/Vulkan)
        adapter = wgpu.request_adapter(
            power_preference="high-performance"
        )

        # Request device
        device = adapter.request_device()

        return WebGPUContext(adapter, device)


class WebGPUContext:
    """Shared WebGPU context for all handlers."""

    def __init__(self, adapter, device):
        self.adapter = adapter
        self.device = device
        self.texture_pool = {}  # Reusable textures
        self.compute_pipelines = {}  # Cached compute pipelines
        self.render_pipelines = {}  # Cached render pipelines

    def create_texture(
        self,
        width: int,
        height: int,
        format: str = 'rgba8unorm'
    ) -> wgpu.GPUTexture:
        """Create GPU texture."""
        return self.device.create_texture(
            size=(width, height, 1),
            format=format,
            usage=(
                wgpu.TextureUsage.COPY_DST |
                wgpu.TextureUsage.TEXTURE_BINDING |
                wgpu.TextureUsage.RENDER_ATTACHMENT
            )
        )

    def create_compute_pipeline(self, shader_code: str):
        """Create compute pipeline from WGSL shader."""
        shader_module = self.device.create_shader_module(code=shader_code)
        return self.device.create_compute_pipeline(
            layout="auto",
            compute={"module": shader_module, "entry_point": "main"}
        )

    def create_render_pipeline(self, vertex_shader: str, fragment_shader: str):
        """Create render pipeline from WGSL shaders."""
        # ... create vertex and fragment shader modules ...
        return self.device.create_render_pipeline(
            layout="auto",
            vertex={"module": vertex_module, "entry_point": "main"},
            fragment={"module": fragment_module, "entry_point": "main"}
        )
```

**All handlers access shared context:**

```python
class BlurFilterWebGPU(StreamHandler):
    async def on_start(self):
        # Access shared GPU context from runtime
        self.gpu_context = self._runtime.gpu_context

        # Load compute shader
        self.blur_pipeline = self.gpu_context.create_compute_pipeline("""
            @group(0) @binding(0) var input_texture: texture_2d<f32>;
            @group(0) @binding(1) var output_texture: texture_storage_2d<rgba8unorm, write>;

            @compute @workgroup_size(8, 8)
            fn main(@builtin(global_invocation_id) global_id: vec3<u32>) {
                // Blur implementation
            }
        """)
```

### Platform-Specific Zero-Copy Optimizations

While WebGPU provides a unified API, platform-specific capture pipelines enable zero-copy paths:

**macOS:** AVFoundation → IOSurface → WebGPU
- Capture to IOSurface-backed CVPixelBuffer
- Import IOSurface directly as WebGPU texture
- Native YUV420 format support
- Zero-copy from camera to GPU

**Linux:** V4L2 → DMA-BUF → WebGPU
- Use V4L2 DMA-BUF export capability
- Import DMA-BUF file descriptor as WebGPU texture
- VA-API integration for hardware decode
- Zero-copy from camera/decoder to GPU

**Windows:** MediaFoundation → D3D11 → WebGPU
- Capture to D3D11 texture via MediaFoundation
- Import D3D11 texture as WebGPU texture
- NV12 format for GPU-friendly encoding
- Zero-copy from camera to GPU

**Common pattern:** Platform APIs provide GPU-backed buffers that WebGPU can import directly without CPU transfers.

### WGSL Shader System

WebGPU uses **WGSL** (WebGPU Shading Language) for compute and rendering.

**Shader types:**
1. **Compute shaders**: General-purpose GPU computing (video effects, audio FFT, ML inference)
2. **Render pipelines**: Graphics rendering (YUV→RGB conversion, compositing, overlays)

**Common use cases:**
- Video effects (blur, sharpen, color grading)
- Audio processing (FFT, convolution, filters)
- Format conversion (YUV→RGB, color space transforms)
- Compositing (alpha blending, multi-layer composition)

**Handler implementation patterns:**
- Video processing (blur, sharpen, color grading) via compute shaders
- Audio processing (FFT, filters) via compute shaders
- Format conversion (YUV→RGB) via render pipelines
- Network I/O (RTP, WebRTC, WebSocket) via platform APIs
- ML inference (ONNX, TensorFlow) via GPU execution providers
- Platform-specific camera capture (AVFoundation, V4L2, MediaFoundation)

**For complete handler examples, see `docs/examples/` and `examples/` directories.**

### GPU-First Benefits

1. **Cross-platform:** WebGPU works on macOS/Windows/Linux/Web
2. **Zero-copy:** Platform-specific optimizations (IOSurface/DMA-BUF/D3D11)
3. **Unified API:** Same API for compute and rendering
4. **AI-friendly:** Simple, modern API easy for AI agents to use
5. **Comprehensive:** Handles video, audio, ML, arbitrary data
6. **Future-proof:** Industry moving toward WebGPU

---
