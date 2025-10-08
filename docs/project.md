# streamlib Project

## Vision

**Composable realtime streaming library for Python with network-transparent operations.**

Unix-pipe-style primitives for realtime streams (video, audio, events, data) that AI agents can orchestrate. Aligned with SMPTE ST 2110 professional broadcast standards.

```python
# Like Unix pipes but for realtime streams
camera >> compositor >> display
audio_gen >> mixer >> speaker
keyboard >> event_handler
```

## Why This Exists

### The Problem
Most streaming/visual tools are large, stateful, monolithic applications (Unity, OBS, streaming platforms). They're environments, not primitives. There's no equivalent to `grep | sed | awk` for realtime visual/audio operations.

### The Solution
**Composable actor-based primitives** - like Unix tools but for realtime streams:

```bash
# Unix philosophy (text)
cat file.txt | grep "error" | sed 's/ERROR/WARNING/' | awk '{print $1}'

# streamlib (realtime streams)
CameraActor() >> CompositorActor() >> DisplayActor()
```

### Core Philosophy
1. **Actor-based**: Each component is an independent actor processing ticks
2. **Tick-driven**: Clock ticks drive processing, not message queues
3. **Ring buffers**: Fixed-size circular buffers, latest-read semantics, zero-copy
4. **SMPTE ST 2110 aligned**: Professional broadcast standards (RTP/UDP, PTP timestamps)
5. **Network-transparent**: Operations work seamlessly locally or remotely
6. **Distributed**: Chain operations across machines (phone → edge → cloud)
7. **Zero-dependency core**: No GStreamer required (uses PyAV for codecs)
8. **AI-orchestratable**: Agents can dynamically create and connect actors

## Architecture Summary

See `docs/architecture.md` and `docs/actor-model-analysis.md` for complete details.

### Key Principles

**Actor Pattern:**
- Actors run continuously from creation (no start/stop)
- Tick-based processing (not message queues)
- Ring buffers for data exchange (3 slots, latest-read)
- Encapsulated state (private to each actor)
- Concurrent and independent execution

**Tick-Based Processing:**
```python
# Clock generates ticks
tick = TimedTick(timestamp=1234.567, frame_number=42)

# Actor receives tick signal
async def process(self, tick: TimedTick):
    # Read latest data from ring buffer
    frame = self.inputs['video'].read_latest()

    # Do work
    result = self.transform(frame)

    # Write to output ring buffer
    self.outputs['video'].write(result)
```

**Ring Buffers (Not Queues):**
```python
# 3-slot circular buffer (matches broadcast practice)
class RingBuffer:
    def __init__(self, slots=3):
        self.buffer = [None] * slots
        self.write_idx = 0

    def write(self, data):
        """Overwrite oldest slot."""
        self.buffer[self.write_idx] = data
        self.write_idx = (self.write_idx + 1) % len(self.buffer)

    def read_latest(self):
        """Read most recent (skip old)."""
        return self.buffer[(self.write_idx - 1) % len(self.buffer)]
```

**SMPTE ST 2110 Alignment:**
- Jitter buffers (1-10ms) for packet reordering
- Ring buffers match professional broadcast practice
- One UDP port per stream (port-per-output principle)
- PTP timestamps in RTP headers
- Control plane (URIs) + data plane (RTP/UDP) separation

**Clock Abstraction:**
```python
# Swappable clock sources
if genlock_signal_present:
    clock = GenlockClock(sdi_port)  # SDI hardware sync
elif ptp_available:
    clock = PTPClock(ptp_client)    # Network PTP
else:
    clock = SoftwareClock(fps=60)   # Free-run
```

**Four Dispatcher Types:**
- `AsyncioDispatcher` - I/O-bound (network, file, events)
- `ThreadPoolDispatcher` - CPU-bound (encoding, audio DSP)
- `ProcessPoolDispatcher` - Heavy compute (multi-pass encoding)
- `GPUDispatcher` - GPU-accelerated (ML inference, shaders)

### Core Abstractions

```python
# Actor - base class for all components
class Actor(ABC):
    inputs: Dict[str, RingBuffer]   # Ring buffers (receive data)
    outputs: Dict[str, RingBuffer]  # Ring buffers (send data)
    clock: Clock                     # Time synchronization
    dispatcher: Dispatcher           # Execution context

    async def process(self, tick: TimedTick):
        """Override: process one tick, read latest from ring buffers"""
        pass

# Message types (stored in ring buffers)
VideoFrame, AudioBuffer, KeyEvent, MouseEvent, DataMessage

# Connection
upstream.outputs['video'] >> downstream.inputs['video']
# Creates ring buffer between actors, wires up tick propagation
```

## Current Status

### Phase 1 & 2: Prototype ✅ COMPLETE (BUT OBSOLETE)

**Legacy code (old architecture):**
- Source/Sink pattern (wrong architecture)
- Queue-based message passing (we use ring buffers)
- Centralized orchestrator (we use distributed actors)
- Manual lifecycle (we auto-start)

**Status:** ~95% rewrite needed. Keep algorithms only (alpha blending math, test patterns, PyAV usage).

### Phase 3: Actor Implementation 🚧 IN PROGRESS (RESTART)

**Goal:** Implement actor model from scratch with SMPTE ST 2110 alignment.

**Start fresh:** New codebase based on architecture docs, not legacy code.

## Phase 3 Implementation Plan

### Implementation Order

**Priority 1: Core Infrastructure** (Foundation)
1. Ring buffers (CPU and GPU)
2. Clock abstraction (PTP/genlock/software)
3. Dispatchers (Asyncio/ThreadPool/ProcessPool/GPU)
4. Actor base class
5. Tick generation and propagation

**Priority 2: Basic Actors** (Proof of concept)
6. TestPatternActor (video generation)
7. DisplayActor (video display)
8. Connection system (>> operator)

**Priority 3: Actor Registry** (Network transparency)
9. URI parser and registry
10. Port allocator
11. Local/remote stubs

**Priority 4: Additional Actors** (Completeness)
12. CompositorActor
13. DrawingActor
14. FileReaderActor / FileWriterActor
15. Audio actors (generator, output)

---

## Detailed Task Breakdown

### 1. Ring Buffers

**Files:** `src/streamlib/buffers.py`

**CPU Ring Buffer:**
```python
class RingBuffer:
    """Fixed-size circular buffer for CPU data."""

    def __init__(self, slots: int = 3):
        self.slots = slots
        self.buffer = [None] * slots
        self.write_idx = 0
        self.lock = threading.Lock()

    def write(self, data: Any) -> None:
        """Overwrite oldest slot (thread-safe)."""
        with self.lock:
            self.buffer[self.write_idx] = data
            self.write_idx = (self.write_idx + 1) % self.slots

    def read_latest(self) -> Any:
        """Read most recent (thread-safe)."""
        with self.lock:
            idx = (self.write_idx - 1) % self.slots
            return self.buffer[idx]

    def is_empty(self) -> bool:
        """Check if any data written."""
        return all(x is None for x in self.buffer)
```

**GPU Ring Buffer (PyTorch):**
```python
import torch

class GPURingBuffer:
    """Zero-copy GPU memory ring buffer."""

    def __init__(self, slots: int = 3, shape: tuple = (1920, 1080, 3)):
        # Pre-allocate GPU buffers
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

**Tests:**
- Write/read from single thread
- Write from one thread, read from another
- Overwrite behavior (oldest slot replaced)
- Latest-read semantics (skips old data)
- GPU buffer zero-copy (no CPU transfer)

**Dependencies:** None (core primitive)

---

### 2. Clock Abstraction

**Files:** `src/streamlib/clocks.py`

**Base Clock:**
```python
from abc import ABC, abstractmethod
from dataclasses import dataclass

@dataclass
class TimedTick:
    """Clock tick with timing information."""
    timestamp: float      # Seconds since epoch (or relative)
    frame_number: int     # Monotonic frame counter
    clock_id: str        # Clock source identifier

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
```

**Software Clock:**
```python
import asyncio
import time

class SoftwareClock(Clock):
    """Free-running software clock (bathtub mode)."""

    def __init__(self, fps: float = 60.0, clock_id: str = 'software'):
        self.fps = fps
        self.period = 1.0 / fps
        self.clock_id = clock_id
        self.frame_number = 0
        self.start_time = time.monotonic()

    async def next_tick(self) -> TimedTick:
        """Generate tick at fixed rate."""
        # Sleep until next frame time
        target_time = self.start_time + (self.frame_number * self.period)
        now = time.monotonic()
        sleep_time = target_time - now

        if sleep_time > 0:
            await asyncio.sleep(sleep_time)

        tick = TimedTick(
            timestamp=time.time(),
            frame_number=self.frame_number,
            clock_id=self.clock_id
        )
        self.frame_number += 1
        return tick

    def get_fps(self) -> float:
        return self.fps
```

**PTP Clock (Stub):**
```python
class PTPClock(Clock):
    """IEEE 1588 Precision Time Protocol clock."""

    def __init__(self, ptp_client: 'PTPClient', fps: float = 60.0):
        self.ptp_client = ptp_client
        self.fps = fps
        self.period = 1.0 / fps
        self.frame_number = 0

    async def next_tick(self) -> TimedTick:
        """Generate tick synced to PTP."""
        # Get PTP time (microsecond accuracy)
        ptp_time = await self.ptp_client.get_time()

        # Sleep until next frame boundary
        next_frame_time = self._next_frame_boundary(ptp_time)
        await asyncio.sleep(next_frame_time - ptp_time)

        tick = TimedTick(
            timestamp=next_frame_time,
            frame_number=self.frame_number,
            clock_id=f'ptp:{self.ptp_client.domain}'
        )
        self.frame_number += 1
        return tick

    def _next_frame_boundary(self, current_time: float) -> float:
        """Calculate next frame boundary aligned to PTP."""
        return ((current_time // self.period) + 1) * self.period

    def get_fps(self) -> float:
        return self.fps
```

**Genlock Clock (Stub):**
```python
class GenlockClock(Clock):
    """SDI hardware sync clock (genlock signal)."""

    def __init__(self, sdi_device: 'SDIDevice'):
        self.sdi_device = sdi_device
        self.frame_number = 0

    async def next_tick(self) -> TimedTick:
        """Wait for genlock pulse."""
        # Block until hardware pulse arrives
        await self.sdi_device.wait_for_pulse()

        tick = TimedTick(
            timestamp=time.time(),
            frame_number=self.frame_number,
            clock_id=f'genlock:{self.sdi_device.port}'
        )
        self.frame_number += 1
        return tick

    def get_fps(self) -> float:
        # Genlock FPS detected from hardware
        return self.sdi_device.detected_fps()
```

**Tests:**
- SoftwareClock generates ticks at correct rate (± 1ms jitter)
- Frame numbers increment monotonically
- PTP clock stub (no real PTP, mock it)
- Genlock clock stub (no real hardware, mock it)

**Dependencies:** None (core primitive)

---

### 3. Dispatchers

**Files:** `src/streamlib/dispatchers.py`

**Base Dispatcher:**
```python
from abc import ABC, abstractmethod
from typing import Callable, Coroutine

class Dispatcher(ABC):
    """Abstract dispatcher for actor execution."""

    @abstractmethod
    async def dispatch(self, coro: Coroutine) -> None:
        """Execute coroutine in appropriate context."""
        pass

    @abstractmethod
    async def shutdown(self) -> None:
        """Clean shutdown."""
        pass
```

**Asyncio Dispatcher:**
```python
import asyncio

class AsyncioDispatcher(Dispatcher):
    """I/O-bound tasks (network, file, events)."""

    def __init__(self):
        self.tasks = set()

    async def dispatch(self, coro: Coroutine) -> None:
        """Execute in asyncio event loop."""
        task = asyncio.create_task(coro)
        self.tasks.add(task)
        task.add_done_callback(self.tasks.discard)

    async def shutdown(self) -> None:
        """Wait for all tasks."""
        if self.tasks:
            await asyncio.gather(*self.tasks, return_exceptions=True)
```

**ThreadPool Dispatcher:**
```python
from concurrent.futures import ThreadPoolExecutor

class ThreadPoolDispatcher(Dispatcher):
    """CPU-bound tasks (encoding, audio DSP)."""

    def __init__(self, max_workers: int = 4):
        self.executor = ThreadPoolExecutor(max_workers=max_workers)
        self.loop = None

    async def dispatch(self, coro: Coroutine) -> None:
        """Execute in thread pool."""
        if self.loop is None:
            self.loop = asyncio.get_running_loop()

        # Run coroutine in thread
        await self.loop.run_in_executor(self.executor, self._run_coro, coro)

    def _run_coro(self, coro: Coroutine):
        """Helper to run coroutine in thread."""
        loop = asyncio.new_event_loop()
        try:
            return loop.run_until_complete(coro)
        finally:
            loop.close()

    async def shutdown(self) -> None:
        """Shutdown thread pool."""
        self.executor.shutdown(wait=True)
```

**ProcessPool Dispatcher (Stub):**
```python
from concurrent.futures import ProcessPoolExecutor

class ProcessPoolDispatcher(Dispatcher):
    """Heavy compute (multi-pass encoding)."""

    def __init__(self, max_workers: int = 2):
        self.executor = ProcessPoolExecutor(max_workers=max_workers)
        # TODO: Implement process pool execution

    async def dispatch(self, coro: Coroutine) -> None:
        raise NotImplementedError("ProcessPool not yet implemented")

    async def shutdown(self) -> None:
        self.executor.shutdown(wait=True)
```

**GPU Dispatcher (Stub):**
```python
class GPUDispatcher(Dispatcher):
    """GPU-accelerated (ML inference, shaders)."""

    def __init__(self, device: str = 'cuda:0'):
        self.device = device
        # TODO: CUDA stream management

    async def dispatch(self, coro: Coroutine) -> None:
        # GPU work happens synchronously within coroutine
        # (PyTorch operations are synchronous)
        await coro

    async def shutdown(self) -> None:
        # TODO: Synchronize CUDA streams
        pass
```

**Tests:**
- AsyncioDispatcher runs coroutines concurrently
- ThreadPoolDispatcher runs in separate threads
- Shutdown waits for completion
- ProcessPool and GPU are stubs (tested later)

**Dependencies:** Ring buffers (for data exchange)

---

### 4. Actor Base Class

**Files:** `src/streamlib/actor.py`

**Actor:**
```python
from abc import ABC, abstractmethod
from typing import Dict, Optional
import asyncio

class Actor(ABC):
    """Base class for all actors."""

    def __init__(
        self,
        actor_id: str,
        clock: Optional[Clock] = None,
        dispatcher: Optional[Dispatcher] = None
    ):
        self.actor_id = actor_id
        self.clock = clock or SoftwareClock()
        self.dispatcher = dispatcher or AsyncioDispatcher()

        # Ring buffers (populated by subclasses)
        self.inputs: Dict[str, RingBuffer] = {}
        self.outputs: Dict[str, RingBuffer] = {}

        # Internal state
        self._running = False
        self._task = None

    def start(self) -> None:
        """Start actor (called automatically in __init__ of subclass)."""
        if not self._running:
            self._running = True
            self._task = asyncio.create_task(self._run())

    async def _run(self) -> None:
        """Internal run loop."""
        try:
            async for tick in self._tick_generator():
                if not self._running:
                    break
                await self.process(tick)
        except Exception as e:
            print(f"[{self.actor_id}] Error: {e}")
            import traceback
            traceback.print_exc()

    async def _tick_generator(self):
        """Generate ticks from clock."""
        while self._running:
            tick = await self.clock.next_tick()
            yield tick

    @abstractmethod
    async def process(self, tick: TimedTick) -> None:
        """Process one tick. Subclasses override this."""
        pass

    async def stop(self) -> None:
        """Stop actor."""
        self._running = False
        if self._task:
            await self._task
```

**StreamInput/Output (Connection helpers):**
```python
class StreamInput:
    """Input port (ring buffer reader)."""

    def __init__(self, name: str):
        self.name = name
        self.buffer: Optional[RingBuffer] = None

    def connect(self, buffer: RingBuffer) -> None:
        """Connect to ring buffer."""
        self.buffer = buffer

    def read_latest(self):
        """Read latest data from ring buffer."""
        if self.buffer is None:
            return None
        return self.buffer.read_latest()

    def is_connected(self) -> bool:
        return self.buffer is not None

class StreamOutput:
    """Output port (ring buffer writer)."""

    def __init__(self, name: str, slots: int = 3):
        self.name = name
        self.buffer = RingBuffer(slots=slots)
        self.subscribers = []

    def write(self, data: Any) -> None:
        """Write data to ring buffer."""
        self.buffer.write(data)

    def __rshift__(self, other: StreamInput):
        """Pipe operator: output >> input"""
        other.connect(self.buffer)
        return other
```

**Tests:**
- Actor starts automatically
- Actor receives ticks at correct rate
- Actor can be stopped
- StreamInput/Output connection works
- >> operator connects buffers

**Dependencies:** Ring buffers, Clock, Dispatchers

---

### 5. TestPatternActor

**Files:** `src/streamlib/actors/video.py`

```python
import numpy as np
from dataclasses import dataclass

@dataclass
class VideoFrame:
    """Video frame message."""
    data: np.ndarray      # Shape: (H, W, 3), dtype: uint8
    timestamp: float
    frame_number: int
    width: int
    height: int

class TestPatternActor(Actor):
    """Generate test patterns (SMPTE bars, gradients, etc)."""

    def __init__(
        self,
        actor_id: str = 'test-pattern',
        width: int = 1920,
        height: int = 1080,
        pattern: str = 'smpte_bars',
        fps: float = 60.0
    ):
        super().__init__(
            actor_id=actor_id,
            clock=SoftwareClock(fps=fps)
        )

        self.width = width
        self.height = height
        self.pattern = pattern

        # Create output port
        self.outputs['video'] = StreamOutput('video')

        # Auto-start
        self.start()

    async def process(self, tick: TimedTick) -> None:
        """Generate frame for this tick."""
        # Generate pattern
        if self.pattern == 'smpte_bars':
            frame_data = self._generate_smpte_bars()
        elif self.pattern == 'gradient':
            frame_data = self._generate_gradient()
        else:
            frame_data = np.zeros((self.height, self.width, 3), dtype=np.uint8)

        # Create frame message
        frame = VideoFrame(
            data=frame_data,
            timestamp=tick.timestamp,
            frame_number=tick.frame_number,
            width=self.width,
            height=self.height
        )

        # Write to output ring buffer
        self.outputs['video'].write(frame)

    def _generate_smpte_bars(self) -> np.ndarray:
        """Generate SMPTE color bars."""
        frame = np.zeros((self.height, self.width, 3), dtype=np.uint8)

        # 7 vertical bars
        bar_width = self.width // 7
        colors = [
            (180, 180, 180),  # White
            (180, 180, 16),   # Yellow
            (16, 180, 180),   # Cyan
            (16, 180, 16),    # Green
            (180, 16, 180),   # Magenta
            (180, 16, 16),    # Red
            (16, 16, 180),    # Blue
        ]

        for i, color in enumerate(colors):
            x_start = i * bar_width
            x_end = (i + 1) * bar_width if i < 6 else self.width
            frame[:, x_start:x_end] = color

        return frame

    def _generate_gradient(self) -> np.ndarray:
        """Generate horizontal gradient."""
        frame = np.zeros((self.height, self.width, 3), dtype=np.uint8)
        gradient = np.linspace(0, 255, self.width, dtype=np.uint8)
        frame[:, :, 0] = gradient  # Red channel
        frame[:, :, 1] = gradient  # Green channel
        frame[:, :, 2] = gradient  # Blue channel
        return frame
```

**Tests:**
- Generates frames at correct FPS
- SMPTE bars look correct (visual verification)
- Gradient looks correct
- Frame numbers increment
- Timestamps advance correctly

**Dependencies:** Actor, Clock, Ring buffers

---

### 6. DisplayActor

**Files:** `src/streamlib/actors/display.py`

```python
import cv2
import asyncio

class DisplayActor(Actor):
    """Display video frames in OpenCV window."""

    def __init__(
        self,
        actor_id: str = 'display',
        window_name: str = 'streamlib',
        inherit_clock: bool = True
    ):
        # Display actors inherit upstream clock (don't generate ticks)
        super().__init__(
            actor_id=actor_id,
            clock=None  # Will be set when connected
        )

        self.window_name = window_name
        self.inherit_clock = inherit_clock

        # Create input port
        self.inputs['video'] = StreamInput('video')

        # Create window
        cv2.namedWindow(self.window_name, cv2.WINDOW_NORMAL)

        # Auto-start
        self.start()

    async def process(self, tick: TimedTick) -> None:
        """Display latest frame."""
        # Read latest frame from ring buffer
        frame = self.inputs['video'].read_latest()

        if frame is None:
            # No frame yet
            return

        # Convert RGB to BGR (OpenCV uses BGR)
        bgr_frame = cv2.cvtColor(frame.data, cv2.COLOR_RGB2BGR)

        # Display
        cv2.imshow(self.window_name, bgr_frame)

        # Non-blocking waitKey
        await asyncio.sleep(0)  # Yield to event loop
        cv2.waitKey(1)

    async def stop(self) -> None:
        """Clean up window."""
        await super().stop()
        cv2.destroyWindow(self.window_name)
```

**Tests:**
- Displays frames (manual verification)
- Doesn't block event loop
- Handles missing frames gracefully
- Closes window on stop

**Dependencies:** Actor, Ring buffers, TestPatternActor (for testing)

---

### 7. Connection System

**Files:** `src/streamlib/connections.py`

```python
def connect(source: Actor, dest: Actor,
            output_name: str = 'video',
            input_name: str = 'video') -> None:
    """Connect two actors."""

    if output_name not in source.outputs:
        raise ValueError(f"Source has no output '{output_name}'")
    if input_name not in dest.inputs:
        raise ValueError(f"Dest has no input '{input_name}'")

    # Connect ring buffer
    source.outputs[output_name] >> dest.inputs[input_name]

    # Transfer clock if dest inherits
    if dest.clock is None and hasattr(dest, 'inherit_clock'):
        dest.clock = source.clock
```

**Pipe operator already implemented in StreamOutput.__rshift__()**

**Tests:**
- Connect TestPatternActor to DisplayActor
- Verify frames flow through ring buffer
- Verify clock inheritance
- Test >> operator
- Test disconnect (if implemented)

**Dependencies:** Actor, StreamInput, StreamOutput

---

### 8. Actor Registry

**Files:** `src/streamlib/registry.py`

```python
from typing import Dict, Optional
from urllib.parse import urlparse

class ActorRegistry:
    """Registry for actor discovery (URI → actor reference)."""

    def __init__(self):
        self.actors: Dict[str, Actor] = {}
        self.port_allocator = PortAllocator(start_port=5000)

    def register(self, uri: str, actor: Actor) -> None:
        """Register actor with URI."""
        self.actors[uri] = actor

    def get(self, uri: str) -> Optional[Actor]:
        """Get actor by URI."""
        return self.actors.get(uri)

    def get_or_create(self, uri: str, factory: callable) -> Actor:
        """Get existing actor or create new one."""
        if uri in self.actors:
            return self.actors[uri]

        actor = factory(uri)
        self.register(uri, actor)
        return actor

    def allocate_port(self, actor_id: str, output_name: str) -> int:
        """Allocate UDP port for actor output."""
        key = f"{actor_id}.{output_name}"
        return self.port_allocator.allocate(key)

class PortAllocator:
    """Allocate UDP ports for SMPTE ST 2110 streams."""

    def __init__(self, start_port: int = 5000):
        self.start_port = start_port
        self.allocated: Dict[str, int] = {}
        self.next_port = start_port

    def allocate(self, key: str) -> int:
        """Allocate port for key."""
        if key in self.allocated:
            return self.allocated[key]

        port = self.next_port
        self.next_port += 2  # Even ports only (SMPTE convention)
        self.allocated[key] = port
        return port

def parse_actor_uri(uri: str) -> dict:
    """Parse actor URI: actor://host/ActorClass/instance-id"""
    parsed = urlparse(uri)

    if parsed.scheme != 'actor':
        raise ValueError(f"Invalid URI scheme: {parsed.scheme}")

    path_parts = parsed.path.strip('/').split('/')
    if len(path_parts) != 2:
        raise ValueError(f"Invalid URI path: {parsed.path}")

    return {
        'host': parsed.netloc or 'local',
        'actor_class': path_parts[0],
        'instance_id': path_parts[1],
    }

# Global registry
_registry = ActorRegistry()

def get_actor(uri: str, factory: Optional[callable] = None) -> Actor:
    """Get or create actor by URI."""
    if factory:
        return _registry.get_or_create(uri, factory)
    return _registry.get(uri)

def register_actor(uri: str, actor: Actor) -> None:
    """Register actor."""
    _registry.register(uri, actor)
```

**Tests:**
- Parse valid URIs
- Reject invalid URIs
- Register and retrieve actors
- Get-or-create pattern
- Port allocation (even ports, no collisions)

**Dependencies:** Actor base class

---

### 9. CompositorActor

**Files:** `src/streamlib/actors/compositor.py`

```python
class CompositorActor(Actor):
    """Composite multiple video streams with alpha blending."""

    def __init__(
        self,
        actor_id: str = 'compositor',
        width: int = 1920,
        height: int = 1080,
        background_color: tuple = (20, 20, 30, 255)
    ):
        super().__init__(actor_id=actor_id)

        self.width = width
        self.height = height
        self.background_color = background_color

        # Create ports
        self.inputs['base'] = StreamInput('base')  # Optional base layer
        self.outputs['video'] = StreamOutput('video')

        # Layer management
        self.layers: List[StreamInput] = []

        self.start()

    def add_layer(self, name: str) -> StreamInput:
        """Add input layer."""
        input_port = StreamInput(name)
        self.inputs[name] = input_port
        self.layers.append(input_port)
        return input_port

    async def process(self, tick: TimedTick) -> None:
        """Composite all layers."""
        # Start with background
        result = self._generate_background()

        # Blend base layer if present
        base_frame = self.inputs['base'].read_latest()
        if base_frame:
            result = self._alpha_blend(result, base_frame.data)

        # Blend each layer
        for layer_input in self.layers:
            layer_frame = layer_input.read_latest()
            if layer_frame:
                result = self._alpha_blend(result, layer_frame.data)

        # Create output frame
        frame = VideoFrame(
            data=result[:, :, :3],  # Drop alpha channel
            timestamp=tick.timestamp,
            frame_number=tick.frame_number,
            width=self.width,
            height=self.height
        )

        self.outputs['video'].write(frame)

    def _generate_background(self) -> np.ndarray:
        """Generate RGBA background."""
        # Reuse existing implementation from compositor.py
        pass

    def _alpha_blend(self, background: np.ndarray, overlay: np.ndarray) -> np.ndarray:
        """Alpha blend overlay onto background."""
        # Reuse existing optimized implementation from compositor.py
        pass
```

**Tests:**
- Composite single layer
- Composite multiple layers
- Alpha blending correctness
- Performance (should match old compositor)

**Dependencies:** Actor, Ring buffers, TestPatternActor

---

### 10. DrawingActor

**Files:** `src/streamlib/actors/drawing.py`

```python
import skia

class DrawingActor(Actor):
    """Execute Python drawing code to generate video frames."""

    def __init__(
        self,
        actor_id: str,
        draw_code: str,
        width: int = 1920,
        height: int = 1080,
        fps: float = 60.0
    ):
        super().__init__(
            actor_id=actor_id,
            clock=SoftwareClock(fps=fps)
        )

        self.width = width
        self.height = height
        self.draw_function = None

        # Compile drawing code
        self._compile_draw_code(draw_code)

        # Create output
        self.outputs['video'] = StreamOutput('video')

        self.start()

    def _compile_draw_code(self, code: str) -> None:
        """Compile drawing code."""
        namespace = {
            'skia': skia,
            'np': np,
        }
        exec(code, namespace)
        self.draw_function = namespace.get('draw')

    async def process(self, tick: TimedTick) -> None:
        """Render frame."""
        # Create Skia surface
        surface = skia.Surface(self.width, self.height)
        canvas = surface.getCanvas()
        canvas.clear(skia.Color(0, 0, 0, 0))

        # Create drawing context
        ctx = DrawingContext(
            time=tick.timestamp,
            frame_number=tick.frame_number,
            width=self.width,
            height=self.height
        )

        # Call user draw function
        if self.draw_function:
            self.draw_function(canvas, ctx)

        # Convert to numpy
        image = surface.makeImageSnapshot()
        array = image.toarray()

        # Create frame
        frame = VideoFrame(
            data=array[:, :, :3],  # Drop alpha
            timestamp=tick.timestamp,
            frame_number=tick.frame_number,
            width=self.width,
            height=self.height
        )

        self.outputs['video'].write(frame)
```

**Tests:**
- Execute drawing code
- Generate animated frames
- Handle errors in draw function
- Performance comparable to old DrawingLayer

**Dependencies:** Actor, Ring buffers, Skia

---

### 11. Audio Actors (Basic)

**Files:** `src/streamlib/actors/audio.py`

```python
@dataclass
class AudioBuffer:
    """Audio buffer message."""
    data: np.ndarray      # Shape: (samples, channels), dtype: float32
    timestamp: float
    sample_rate: int
    channels: int

class AudioGeneratorActor(Actor):
    """Generate audio tones."""

    def __init__(
        self,
        actor_id: str = 'audio-gen',
        sample_rate: int = 48000,
        channels: int = 2,
        frequency: float = 440.0  # A4
    ):
        super().__init__(
            actor_id=actor_id,
            clock=SoftwareClock(fps=50)  # 20ms audio chunks
        )

        self.sample_rate = sample_rate
        self.channels = channels
        self.frequency = frequency
        self.samples_per_chunk = sample_rate // 50  # 20ms
        self.phase = 0.0

        self.outputs['audio'] = StreamOutput('audio')
        self.start()

    async def process(self, tick: TimedTick) -> None:
        """Generate audio chunk."""
        # Generate sine wave
        t = np.arange(self.samples_per_chunk) / self.sample_rate
        signal = np.sin(2 * np.pi * self.frequency * (t + self.phase))
        self.phase += self.samples_per_chunk / self.sample_rate

        # Stereo (duplicate to both channels)
        audio_data = np.stack([signal, signal], axis=1).astype(np.float32)

        # Create buffer
        buffer = AudioBuffer(
            data=audio_data,
            timestamp=tick.timestamp,
            sample_rate=self.sample_rate,
            channels=self.channels
        )

        self.outputs['audio'].write(buffer)
```

**Tests:**
- Generate sine wave
- Correct sample rate
- Correct duration (20ms chunks)

**Dependencies:** Actor, Ring buffers

---

### 12. File I/O Actors

**Files:** `src/streamlib/actors/io.py`

```python
import av

class FileReaderActor(Actor):
    """Read video from file using PyAV."""

    def __init__(
        self,
        actor_id: str,
        file_path: str,
        loop: bool = True
    ):
        # Detect FPS from file
        container = av.open(file_path)
        stream = container.streams.video[0]
        fps = float(stream.average_rate)
        container.close()

        super().__init__(
            actor_id=actor_id,
            clock=SoftwareClock(fps=fps)
        )

        self.file_path = file_path
        self.loop = loop
        self.container = None
        self.stream = None

        self.outputs['video'] = StreamOutput('video')
        self._open_file()
        self.start()

    def _open_file(self):
        """Open video file."""
        self.container = av.open(self.file_path)
        self.stream = self.container.streams.video[0]
        self.frame_iter = self.container.decode(self.stream)

    async def process(self, tick: TimedTick) -> None:
        """Read next frame."""
        try:
            av_frame = next(self.frame_iter)
            array = av_frame.to_ndarray(format='rgb24')

            frame = VideoFrame(
                data=array,
                timestamp=tick.timestamp,
                frame_number=tick.frame_number,
                width=array.shape[1],
                height=array.shape[0]
            )

            self.outputs['video'].write(frame)

        except StopIteration:
            if self.loop:
                # Reopen file
                self.container.close()
                self._open_file()
            else:
                # Stop actor
                await self.stop()

class FileWriterActor(Actor):
    """Write video to file using PyAV."""

    def __init__(
        self,
        actor_id: str,
        file_path: str,
        width: int = 1920,
        height: int = 1080,
        fps: float = 60.0,
        codec: str = 'libx264'
    ):
        super().__init__(actor_id=actor_id)

        self.file_path = file_path
        self.width = width
        self.height = height
        self.fps = fps

        # Create output container
        self.container = av.open(file_path, 'w')
        self.stream = self.container.add_stream(codec, rate=fps)
        self.stream.width = width
        self.stream.height = height
        self.stream.pix_fmt = 'yuv420p'

        self.inputs['video'] = StreamInput('video')
        self.start()

    async def process(self, tick: TimedTick) -> None:
        """Write frame to file."""
        frame = self.inputs['video'].read_latest()
        if frame is None:
            return

        # Convert to AVFrame
        av_frame = av.VideoFrame.from_ndarray(frame.data, format='rgb24')

        # Encode
        for packet in self.stream.encode(av_frame):
            self.container.mux(packet)

    async def stop(self) -> None:
        """Flush and close file."""
        # Flush remaining packets
        for packet in self.stream.encode():
            self.container.mux(packet)

        self.container.close()
        await super().stop()
```

**Tests:**
- Read video file
- Write video file
- Loop functionality
- Verify written file is playable

**Dependencies:** Actor, Ring buffers, PyAV

---

## Testing Strategy

### Unit Tests

Each component tested in isolation:
- Ring buffers (CPU and GPU)
- Clocks (Software, PTP stub, Genlock stub)
- Dispatchers (Asyncio, ThreadPool)
- Actor base class
- Actor registry
- Each actor implementation

### Integration Tests

Components working together:
- TestPatternActor → DisplayActor (basic pipeline)
- TestPatternActor → CompositorActor → DisplayActor
- DrawingActor → DisplayActor
- FileReaderActor → DisplayActor
- TestPatternActor → FileWriterActor (verify file)
- Multi-stream (video + audio together)

### Performance Tests

Realtime requirements:
- 1080p60: < 16ms per frame (P99)
- Jitter: < 1ms (P99 - P50)
- CPU usage: < 5% per actor
- Memory: Ring buffers fixed size

### Visual Tests

Manual verification:
- SMPTE bars look correct
- Gradients look smooth
- Alpha blending looks correct
- Drawing animations smooth
- No dropped frames (when within budget)

---

## Documentation

### For Developers

- `docs/architecture.md` - Complete architecture
- `docs/actor-model-analysis.md` - Actor model decisions
- `docs/project.md` - This file (implementation plan)
- API documentation (docstrings)

### For Users

- README.md - Quick start
- examples/ - Working examples
- Tutorial (step-by-step actor creation)

### For AI Agents

- SDK-friendly documentation
- Actor creation patterns
- Connection examples
- Error handling guide

---

## Timeline

### Week 1: Core Infrastructure
- Days 1-2: Ring buffers + tests
- Days 3-4: Clocks + tests
- Days 5-7: Dispatchers + Actor base class + tests

### Week 2: Basic Actors
- Days 1-2: TestPatternActor + DisplayActor + tests
- Day 3: Connection system + integration tests
- Days 4-5: Actor registry + tests
- Days 6-7: CompositorActor + tests

### Week 3: Additional Actors
- Days 1-2: DrawingActor + tests
- Days 3-4: File I/O actors + tests
- Days 5-7: Audio actors + tests

### Week 4: Polish & Profiling
- Days 1-2: Integration testing
- Days 3-4: Performance profiling
- Days 5-6: Documentation
- Day 7: Demo applications

**Total: ~4 weeks to Phase 3 complete**

---

## Phase 4+ (Future)

### Network Transparency
- NetworkSendActor / NetworkReceiveActor
- SMPTE ST 2110 RTP/UDP implementation
- PTP clock synchronization (real implementation)
- Jitter buffer implementation

### Advanced Actors
- WebcamActor (platform-specific)
- ScreenCaptureActor
- ML actors (face detection, object tracking)
- Audio effects actors

### Production Features
- Supervisor pattern (failure recovery)
- Hot reloading actors
- Distributed registry (service discovery)
- Monitoring and metrics
- Performance optimizations (Rust migration if needed)

---

## Success Criteria

Phase 3 is complete when:

1. ✅ All core infrastructure implemented and tested
2. ✅ Basic actors work (TestPattern, Display, Compositor, Drawing)
3. ✅ Connection system works (>> operator)
4. ✅ Actor registry works (URI-based)
5. ✅ Integration tests pass (multi-actor pipelines)
6. ✅ Performance meets targets (1080p60 < 16ms)
7. ✅ Documentation complete
8. ✅ Demo applications work
9. ✅ Zero regressions from Phase 1/2 algorithms (alpha blending, etc.)

---

## Philosophy

> "Composable primitives that AI can orchestrate."

We're not building a streaming platform. We're building **tools** that agents can reason about and combine in novel ways.

Key insights:
- **Emergent behaviors** from simple tools
- **Network-transparent** from day one
- **SMPTE-aligned** for professional workflows
- **Python API** for AI accessibility
- **Rust core** (if needed) for performance

## References

- Architecture: `docs/architecture.md`
- Actor Model Analysis: `docs/actor-model-analysis.md`
- CLAUDE.md: Project vision and context
- SMPTE ST 2110: https://www.smpte.org/
- Actor Model: https://en.wikipedia.org/wiki/Actor_model
