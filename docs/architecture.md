# streamlib Architecture

## Core Philosophy

**streamlib is a composable streaming library based on the Actor Pattern.**

Like Unix tools (`grep | sed | awk`) but for realtime streams (video, audio, events, data). Each component is an **Actor (Node)** that:
- Runs continuously and independently
- Processes messages from mailboxes (input streams)
- Sends messages to other actors (output streams)
- Maintains private encapsulated state
- Requires no shared memory or locks

**Key Metaphor: Water and Boats**
- **Stream = Always flowing** (clock ticks continuously whether anyone participates or not)
- **Boat = Actor/Node** (drops into stream, floats along, processes as it goes)
- **Bathtub = Isolated actor** (creates own clock when not connected to upstream)
- **Upstream/Downstream = Message flow** (actors affect downstream, unaware of upstream)

## Actor Pattern Principles

### 1. Actors Are Always Running
```python
# Wrong - manual lifecycle
node = VideoNode()
await node.start()  # ❌ No start()
await node.run()    # ❌ No run()
await node.stop()   # ❌ No stop()

# Right - actor exists and runs
node = VideoNode()  # ✅ Created and immediately processing
```

When an actor is created, it immediately begins processing messages. The stream (clock) flows whether the actor has anything to do or not. Empty mailbox? Actor just waits for next tick.

### 2. Message-Based Communication Only
```python
# Wrong - shared state
compositor.frame = frame  # ❌ Direct state access

# Right - send message
upstream.output >> downstream.input  # ✅ Messages only
```

Actors never share state. All communication via asynchronous messages through input/output streams.

### 3. Mailbox (Queue) Processing
```python
class Actor:
    def __init__(self):
        self.inputs = {
            'video': StreamInput('video', VideoFrame),  # Mailbox
            'audio': StreamInput('audio', AudioBuffer)   # Mailbox
        }

    async def _process_loop(self):
        """Internal loop - processes messages sequentially."""
        async for tick in self.clock.tick():
            # Process messages from mailboxes
            for input in self.inputs.values():
                while not input.queue.empty():
                    msg = await input.read()
                    await self._handle_message(msg, tick)
```

Each input is a mailbox. Messages arrive asynchronously but are processed sequentially, one at a time.

### 4. Encapsulated State
```python
class CompositorActor(Actor):
    def __init__(self):
        super().__init__()
        self._layers = []        # Private state
        self._background = None  # Private state
        # Nobody can access these except this actor
```

State is private. Only the actor can modify its own state.

### 5. Concurrent and Independent
```python
# Actors run concurrently, independently
video_gen = VideoGeneratorActor()
audio_gen = AudioGeneratorActor()
compositor = CompositorActor()
display = DisplayActor()

# All running concurrently from creation
# No asyncio.gather() needed - they're already running
```

## Core Abstractions

### Actor (Base Class)

```python
from abc import ABC, abstractmethod
from enum import Enum

class LifecycleState(Enum):
    """Actor lifecycle states."""
    RUNNING = "running"
    STOPPED = "stopped"
    FAILED = "failed"

class Actor(ABC):
    """
    Base class for all actors in the system.

    An actor:
    - Runs continuously from creation
    - Processes messages from input mailboxes
    - Sends messages to output streams
    - Maintains private state
    - Syncs to upstream clock or creates own
    """

    def __init__(self, parent: Optional['Actor'] = None, dispatcher: Optional['Dispatcher'] = None):
        # Lifecycle
        self.state = LifecycleState.RUNNING
        self.parent = parent
        self.children: List['Actor'] = []

        # Register with parent
        if parent:
            parent.children.append(self)

        # Named inputs (mailboxes)
        self.inputs: Dict[str, StreamInput] = {}

        # Named outputs (message sending)
        self.outputs: Dict[str, StreamOutput] = {}

        # Clock (synced or own)
        self.clock: Optional[Clock] = None

        # Dispatcher (execution model)
        self.dispatcher = dispatcher or AsyncioDispatcher()

        # Start processing immediately
        asyncio.create_task(self._run())

    async def _run(self):
        """
        Internal processing loop - runs until stopped/failed.
        DO NOT override this. Override process() instead.
        """
        try:
            # Initialize dispatcher
            await self.dispatcher.start()

            # Call user hook
            await self.on_start()

            # Get clock (synced from upstream or create own)
            if not self.clock:
                self.clock = await self._acquire_clock()

            # Process until stopped
            async for tick in self.clock.tick():
                if self.state != LifecycleState.RUNNING:
                    break

                # Schedule via dispatcher (enables concurrent execution)
                await self.dispatcher.schedule(self, tick)

            # Stopped gracefully
            if self.state == LifecycleState.RUNNING:
                self.state = LifecycleState.STOPPED

        except Exception as e:
            # Failed
            self.state = LifecycleState.FAILED
            raise

        finally:
            # Cleanup
            await self._cleanup_children()
            await self.on_stop()
            await self.dispatcher.stop()

    async def _acquire_clock(self) -> Clock:
        """
        Acquire clock - either from upstream or create own.

        If any input has received a message with clock metadata,
        sync to that clock. Otherwise create own (bathtub mode).
        """
        # Check if any input has clock metadata
        for input in self.inputs.values():
            if input.queue.qsize() > 0:
                # Peek at first message (don't remove)
                msg = await input.queue.get()
                await input.queue.put(msg)  # Put back

                if hasattr(msg, 'clock_source_id') and msg.clock_source_id:
                    # Sync to upstream clock
                    return UpstreamClock(input)

        # No upstream clock, create own (bathtub)
        return SoftwareClock(fps=self.default_fps)

    async def _cleanup_children(self):
        """Stop all children when parent stops."""
        for child in self.children:
            child.stop()

    # ===== REQUIRED: Override in subclass =====

    @abstractmethod
    async def process(self, tick: TimedTick):
        """
        Process one tick - OVERRIDE THIS.

        Read from self.inputs (mailboxes)
        Do computation
        Write to self.outputs (send messages)

        Called every tick while actor is RUNNING.
        """
        pass

    # ===== OPTIONAL: Override if needed =====

    async def on_start(self):
        """
        Called once when actor starts - OVERRIDE if needed.

        Initialize resources, open files, etc.
        """
        pass

    async def on_stop(self):
        """
        Called when actor stops - OVERRIDE if needed.

        Cleanup resources, close files, etc.
        """
        pass

    @property
    def default_fps(self) -> float:
        """
        Default FPS for bathtub mode - OVERRIDE if needed.

        Only used when actor has no upstream clock.
        """
        return 60.0

    # ===== PROVIDED: Don't override =====

    def spawn(self, actor_class, *args, supervisor=None, **kwargs):
        """
        Spawn child actor. Parent death → children stop.

        Args:
            actor_class: Actor class to instantiate
            supervisor: Optional supervisor for child
            *args, **kwargs: Arguments to actor_class

        Returns:
            Child actor instance
        """
        child = actor_class(*args, parent=self, **kwargs)

        # Optional supervision
        if supervisor:
            supervisor.monitor(child)

        return child

    def stop(self):
        """
        Request actor to stop gracefully.

        Sets state to STOPPED, which exits processing loop.
        """
        self.state = LifecycleState.STOPPED
```

### StreamInput (Mailbox)

```python
class StreamInput:
    """
    Mailbox for receiving messages of a specific type.

    For realtime systems, mailboxes have bounded capacity.
    When full, upstream messages are dropped (not queued).
    """
    def __init__(self, name: str, msg_type: Type, maxsize: int = 2):
        self.name = name
        self.msg_type = msg_type
        # Small queue for realtime - prefer dropping old messages
        # over buffering (which adds latency)
        self.queue = asyncio.Queue(maxsize=maxsize)

    async def read(self) -> Any:
        """Read next message (blocks if empty)."""
        return await self.queue.get()

    async def try_read(self) -> Optional[Any]:
        """Try to read (returns None if empty)."""
        try:
            return self.queue.get_nowait()
        except asyncio.QueueEmpty:
            return None

    def has_messages(self) -> bool:
        """Check if mailbox has messages."""
        return self.queue.qsize() > 0
```

### StreamOutput (Message Sender)

```python
class StreamOutput:
    """
    Output that sends messages to connected mailboxes.
    """
    def __init__(self, name: str, msg_type: Type):
        self.name = name
        self.msg_type = msg_type
        self.subscribers: List[StreamInput] = []  # Connected mailboxes

    async def send(self, message: Any):
        """
        Send message to all subscribers.

        In realtime mode, drops messages if mailbox is full (no blocking).
        This prevents upstream actors from stalling on slow downstream actors.
        """
        for subscriber in self.subscribers:
            try:
                subscriber.queue.put_nowait(message)
            except asyncio.QueueFull:
                # Drop message for realtime - old data is worthless
                # Can add metrics/logging here if needed
                pass

    def connect(self, mailbox: StreamInput):
        """Connect to another actor's mailbox."""
        if mailbox.msg_type != self.msg_type:
            raise TypeError(f"Type mismatch: {self.msg_type} vs {mailbox.msg_type}")
        self.subscribers.append(mailbox)

    def __rshift__(self, mailbox: StreamInput):
        """Pipe operator: output >> input"""
        self.connect(mailbox)
        return mailbox
```

## Message Types

```python
@dataclass
class VideoFrame:
    """Video frame message."""
    data: NDArray[np.uint8]  # RGB or RGBA
    timestamp: float
    frame_number: int
    clock_source_id: Optional[str] = None  # For clock sync
    clock_rate: Optional[float] = None


@dataclass
class AudioBuffer:
    """Audio buffer message."""
    data: NDArray[np.float32]  # Audio samples
    sample_rate: int
    timestamp: float
    clock_source_id: Optional[str] = None


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


@dataclass
class DataMessage:
    """Generic data message."""
    data: bytes
    timestamp: float
```

## Clock Synchronization

### Bathtub Mode (Isolated Actor)
```python
# Actor with no inputs creates own clock
actor = VideoGeneratorActor()
# Automatically creates SoftwareClock at default_fps
# Runs independently
```

### Stream Mode (Connected Actors)
```python
# Upstream actor
generator = VideoGeneratorActor()  # Creates own clock

# Downstream actor
display = DisplayActor()  # No clock yet

# Connect
generator.outputs['video'] >> display.inputs['video']

# When display receives first frame, it syncs to generator's clock
# Clock metadata travels with VideoFrame messages
```

### Multiple Upstreams
```python
compositor = CompositorActor()

# Connect multiple inputs
gen1.outputs['video'] >> compositor.inputs['video']
gen2.outputs['video'] >> compositor.inputs['layer1']

# Compositor syncs to first input that sends a message
# Others are sampled as available
```

## Example Actors

### Video Generator Actor
```python
class VideoGeneratorActor(Actor):
    """
    Generates test pattern video frames.

    Inputs: None
    Outputs:
        - 'video': VideoFrame stream
    """

    def __init__(self, pattern='smpte_bars', width=1920, height=1080, fps=60):
        super().__init__()
        self.pattern = pattern
        self.width = width
        self.height = height
        self._fps = fps

        # Register outputs
        self.outputs['video'] = StreamOutput('video', VideoFrame)

    @property
    def default_fps(self) -> float:
        return self._fps

    async def process(self, tick: TimedTick):
        # Generate frame
        frame_data = self._generate_pattern(tick)

        # Create message
        msg = VideoFrame(
            data=frame_data,
            timestamp=tick.timestamp,
            frame_number=tick.frame_number,
            clock_source_id=id(self.clock),
            clock_rate=self._fps
        )

        # Send to all subscribers
        await self.outputs['video'].send(msg)

    def _generate_pattern(self, tick: TimedTick) -> NDArray:
        # Pattern generation logic
        ...
```

### Display Actor
```python
class DisplayActor(Actor):
    """
    Displays video and emits input events.

    Inputs:
        - 'video': VideoFrame to display
    Outputs:
        - 'keyboard': KeyEvent stream
        - 'mouse': MouseEvent stream
    """

    def __init__(self, window_name='Display'):
        super().__init__()
        self.window_name = window_name

        # Inputs (mailboxes)
        self.inputs['video'] = StreamInput('video', VideoFrame)

        # Outputs (event emission)
        self.outputs['keyboard'] = StreamOutput('keyboard', KeyEvent)
        self.outputs['mouse'] = StreamOutput('mouse', MouseEvent)

        # Initialize display
        cv2.namedWindow(self.window_name, cv2.WINDOW_NORMAL)

    async def process(self, tick: TimedTick):
        # Read video frame from mailbox
        frame = await self.inputs['video'].try_read()

        if frame:
            # Display it
            cv2.imshow(self.window_name, frame.data)

        # Check for input events
        await asyncio.sleep(0)  # Yield to event loop
        key = cv2.waitKey(1)

        if key != -1:
            # Send keyboard event
            event = KeyEvent(key=key, timestamp=tick.timestamp)
            await self.outputs['keyboard'].send(event)
```

### Compositor Actor
```python
class CompositorActor(Actor):
    """
    Composites video layers.

    Inputs:
        - 'base' (optional): Base video layer
        - 'layer_*' (dynamic): Additional layers
    Outputs:
        - 'video': Composited result
    """

    def __init__(self, width=1920, height=1080):
        super().__init__()
        self.width = width
        self.height = height
        self._layers = []

        # Register default inputs/outputs
        self.inputs['base'] = StreamInput('base', VideoFrame)
        self.outputs['video'] = StreamOutput('video', VideoFrame)

    def add_layer(self, layer):
        """Add drawing layer (private state)."""
        self._layers.append(layer)

    async def process(self, tick: TimedTick):
        # Read base frame (optional)
        base = await self.inputs['base'].try_read()

        # Composite layers
        result = await self._composite(base, tick)

        # Send output
        msg = VideoFrame(
            data=result,
            timestamp=tick.timestamp,
            frame_number=tick.frame_number,
            clock_source_id=tick.clock_source,
            clock_rate=tick.clock_rate
        )

        await self.outputs['video'].send(msg)

    async def _composite(self, base, tick):
        # Compositing logic
        ...
```

## Composition Patterns

### Direct Connection
```python
# Create actors (immediately running)
gen = VideoGeneratorActor()
comp = CompositorActor()
disp = DisplayActor()

# Connect (pipe operator)
gen.outputs['video'] >> comp.inputs['base']
comp.outputs['video'] >> disp.inputs['video']

# That's it - already running
```

### Event Handling
```python
class LoggerActor(Actor):
    """Logs keyboard events."""
    def __init__(self):
        super().__init__()
        self.inputs['keys'] = StreamInput('keys', KeyEvent)

    async def process(self, tick: TimedTick):
        event = await self.inputs['keys'].try_read()
        if event:
            print(f"Key pressed: {event.key}")

# Connect display events to logger
display.outputs['keyboard'] >> logger.inputs['keys']
```

### Multi-Stream
```python
# Video path
video_gen >> compositor >> display

# Audio path
audio_gen >> audio_mixer >> audio_output

# Event path
display.outputs['keyboard'] >> key_handler
display.outputs['mouse'] >> mouse_handler
```

### Explicit Buffering
```python
# By default, messages drop when downstream is slow (realtime)
fast_gen >> slow_processor  # Frames will be dropped

# Add explicit buffer actor when needed
class BufferActor(Actor):
    """Buffers messages between fast upstream and slow downstream."""
    def __init__(self, capacity: int = 30):
        super().__init__()
        self.inputs['in'] = StreamInput('in', VideoFrame, maxsize=capacity)
        self.outputs['out'] = StreamOutput('out', VideoFrame)

    async def process(self, tick: TimedTick):
        # Pass through with larger queue
        msg = await self.inputs['in'].try_read()
        if msg:
            await self.outputs['out'].send(msg)

# Use buffer for processing that can't keep up
fast_gen >> BufferActor(capacity=30) >> slow_ml_processor >> display
```

### Network-Transparent (SMPTE ST 2110)

**Design Philosophy:**
- Align with professional broadcast standards (SMPTE ST 2110)
- Minimal, embeddable core (for agent chips on edge devices)
- Interoperable with real broadcast equipment
- Discovery/registry is higher-layer convenience (Phase 5+)

**OSI Model Alignment:**
```
Layer 7: Actor system, composition, orchestration
Layer 6: SMPTE 2110 payload formats (video, audio, data)
Layer 5: RTP (Real-time Transport Protocol)
Layer 4: UDP (connectionless, low latency)
Layer 3: IP (routing)
Layer 2: Ethernet (MAC)
Layer 1: Physical (cables, radio)
```

**Implementation:**
```python
# Minimal RTP actors - just UDP + SMPTE 2110
class RTPSendActor(Actor):
    """
    Send SMPTE ST 2110 RTP stream.

    Encodes VideoFrame/AudioBuffer to SMPTE 2110 RTP packets.
    Sends via UDP to specified host:port.
    """
    def __init__(self, host: str, port: int, payload_type: str = 'video'):
        super().__init__()
        self.inputs['stream'] = StreamInput('stream', VideoFrame)
        self.socket = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        self.dest = (host, port)
        self.sequence = 0

    async def process(self, tick: TimedTick):
        frame = await self.inputs['stream'].try_read()
        if frame:
            # Encode to SMPTE 2110 RTP packet
            rtp_packet = encode_smpte_2110_rtp(
                payload=frame.data,
                timestamp=frame.timestamp,
                sequence=self.sequence,
                ptp_time=frame.ptp_time,
                clock_source=frame.clock_source_id
            )
            self.socket.sendto(rtp_packet, self.dest)
            self.sequence += 1


class RTPReceiveActor(Actor):
    """
    Receive SMPTE ST 2110 RTP stream.

    Receives RTP packets via UDP on specified port.
    Decodes to VideoFrame/AudioBuffer.
    """
    def __init__(self, port: int, payload_type: str = 'video'):
        super().__init__()
        self.outputs['stream'] = StreamOutput('stream', VideoFrame)
        self.socket = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        self.socket.bind(('0.0.0.0', port))
        self.socket.setblocking(False)  # Non-blocking

    async def process(self, tick: TimedTick):
        try:
            # Non-blocking receive
            rtp_packet, addr = self.socket.recvfrom(65536)

            # Decode SMPTE 2110 RTP packet
            frame = decode_smpte_2110_rtp(rtp_packet)

            await self.outputs['stream'].send(frame)
        except BlockingIOError:
            # No packet available, continue
            pass


# Usage - manual addressing (no discovery)
# Local machine
video_gen = VideoGeneratorActor()
rtp_send = RTPSendActor(host='192.168.1.100', port=5004)

video_gen >> rtp_send

# Remote machine (or real SMPTE equipment!)
rtp_recv = RTPReceiveActor(port=5004)
display = DisplayActor()

rtp_recv >> display

# Clock and PTP timestamps flow automatically in RTP headers
```

**SMPTE 2110 Payload Formats (Phase 4):**
- ST 2110-20: Uncompressed video (raw pixels)
- ST 2110-22: Compressed video (H.264, JPEG XS)
- ST 2110-30: PCM audio (uncompressed)
- ST 2110-40: Ancillary data (metadata, subtitles)

**Network Configuration:**
AI agents can retrieve IP/port information and configure RTP actors directly. Manual addressing is simple and explicit.

### Actor Addressing & Discovery

**URI-based addressing (Durable Objects-style):**

Actors referenced by URIs, automatically created on demand:

```
actor://host/ActorClass/instance-id
```

**Control plane vs data plane:**
- **Layer 7 (Control)**: Actor URIs for lifecycle, control messages, registry lookup
- **Layers 4-6 (Data)**: SMPTE ST 2110 RTP/UDP for media streams

**One UDP port per output:**
```python
# Get or create actor by URI
camera = get_actor('actor://edge/VideoCapture/cam-1')
# Actor output bound to: edge:5004

# Stereo camera (multiple outputs)
stereo = get_actor('actor://edge/StereoCam/stereo-1')
stereo.outputs['left_video']   # → edge:5004
stereo.outputs['right_video']  # → edge:5006
stereo.outputs['audio']        # → edge:5008
# Each output = separate SMPTE ST 2110 RTP stream = unique UDP port
```

**Registry maps outputs to ports:**
```python
{
    'cam-1.video': 'edge:5004',
    'stereo-1.left_video': 'edge:5004',
    'stereo-1.right_video': 'edge:5006',
    'stereo-1.audio': 'edge:5008',
    'proc-1.video': 'gpu:5010'
}
```

**Connection flow:**
```python
# 1. Get/create actors (allocates UDP ports)
camera = get_actor('actor://edge/VideoCapture/cam-1')
processor = get_actor('actor://gpu/FaceDetect/proc-1')

# 2. Connect (sets up RTP streams)
camera >> processor

# Behind the scenes:
# - Registry lookup: cam-1.video → edge:5004
# - Registry lookup: proc-1.video → gpu:5010
# - Configure: edge:5004 sends RTP to gpu:5010
```

**Location transparency:**
```python
# Local actor (same process)
display = get_actor('actor://local/Display/main')
# Returns: Direct Python object reference

# Remote actor (different host)
camera = get_actor('actor://edge/VideoCapture/cam-1')
# Returns: Remote stub (proxy object)
# Control messages sent over network
# Media streams use SMPTE ST 2110 RTP/UDP
```

**SMPTE interoperability:**
```python
# Connect to real SMPTE equipment (no URI, just IP:port)
processor = get_actor('actor://gpu/Processor/proc-1')
processor.inputs['video'].receive_from('192.168.1.50:5004')  # Real camera
processor.outputs['video'].send_to('192.168.1.60:5006')      # Real display
```

### Agent-Generated Actors
```python
# Agent generates code for custom actor
code = await agent.generate("""
Create an actor that:
- Reads video frames
- Detects faces
- Emits bounding box data
""")

# Execute code to create actor class
CustomActor = eval(code)

# Instantiate and connect
detector = CustomActor()
video_gen >> detector
detector.outputs['boxes'] >> overlay_actor
```

## Concurrent Execution & Dispatchers

### Why Concurrency Matters

Realtime streams (video, audio, events, data) have different processing characteristics:
- **I/O-bound**: Network streams, file reading, display rendering
- **CPU-bound**: Video encoding, audio processing, data transformation
- **GPU-bound**: ML inference, face detection, shader effects, image processing

A single-threaded asyncio event loop works well for I/O-bound actors but becomes a bottleneck for CPU/GPU-intensive work. **Dispatchers** abstract the execution model, allowing actors to run on the most appropriate executor.

### Dispatcher Abstraction

```python
from abc import ABC, abstractmethod

class Dispatcher(ABC):
    """
    Dispatchers schedule actor message processing on different executors.

    Each actor can specify its preferred dispatcher based on its workload.
    """
    @abstractmethod
    async def schedule(self, actor: Actor, tick: TimedTick):
        """Schedule actor to process one tick."""
        pass

    @abstractmethod
    async def start(self):
        """Initialize dispatcher resources."""
        pass

    @abstractmethod
    async def stop(self):
        """Cleanup dispatcher resources."""
        pass
```

### AsyncioDispatcher (Default - I/O Bound)

**Use for:** Network streams, file I/O, display rendering, event handling

**Characteristics:**
- Single asyncio event loop (one thread)
- Thousands of actors with minimal overhead
- Efficient for I/O wait (network, disk, display)
- **Not suitable** for CPU-intensive work (blocks event loop)

```python
class AsyncioDispatcher(Dispatcher):
    """Default dispatcher for I/O-bound actors."""

    async def schedule(self, actor: Actor, tick: TimedTick):
        """Execute directly on event loop."""
        await actor.process(tick)

    async def start(self):
        pass  # Uses existing event loop

    async def stop(self):
        pass

# Usage (default)
video_stream = RTPReceiveActor()  # I/O-bound, uses AsyncioDispatcher
display = DisplayActor()  # I/O-bound, uses AsyncioDispatcher
```

### ThreadPoolDispatcher (CPU-Bound)

**Use for:** Video encoding/decoding, audio processing, data transformation, compression

**Characteristics:**
- Multiple Python threads (typically 4-8)
- Subject to GIL (Global Interpreter Lock)
- **Good for C extensions** (numpy, OpenCV, PyAV bypass GIL)
- Lower overhead than ProcessPoolDispatcher

```python
class ThreadPoolDispatcher(Dispatcher):
    """Dispatcher for CPU-bound actors using thread pool."""

    def __init__(self, workers: int = 4):
        self.executor = ThreadPoolExecutor(max_workers=workers)
        self.loop = None

    async def schedule(self, actor: Actor, tick: TimedTick):
        """Execute on thread pool."""
        self.loop = self.loop or asyncio.get_event_loop()

        # Run actor.process() on thread pool
        await self.loop.run_in_executor(
            self.executor,
            lambda: asyncio.run(actor.process(tick))
        )

    async def start(self):
        pass

    async def stop(self):
        self.executor.shutdown(wait=True)

# Usage
encoder = VideoEncoderActor(
    dispatcher=ThreadPoolDispatcher(workers=4)
)

# Chain with I/O actors
video_stream >> encoder >> rtp_send
```

### ProcessPoolDispatcher (Heavy CPU)

**Use for:** Heavy encoding (H.264, H.265), complex ML models (if not GPU), batch processing

**Characteristics:**
- Multiple Python processes (typically 2-4)
- Bypasses GIL (true parallelism)
- Higher overhead (process spawn, IPC)
- **Best for compute-heavy actors** with large per-message processing time

```python
class ProcessPoolDispatcher(Dispatcher):
    """Dispatcher for heavy CPU-bound actors using process pool."""

    def __init__(self, workers: int = 2):
        self.executor = ProcessPoolExecutor(max_workers=workers)
        self.loop = None

    async def schedule(self, actor: Actor, tick: TimedTick):
        """Execute on process pool."""
        self.loop = self.loop or asyncio.get_event_loop()

        # Serialize message, execute in process, deserialize result
        await self.loop.run_in_executor(
            self.executor,
            _process_in_worker,
            actor, tick
        )

    async def start(self):
        pass

    async def stop(self):
        self.executor.shutdown(wait=True)

# Usage
heavy_encoder = H265EncoderActor(
    dispatcher=ProcessPoolDispatcher(workers=2)
)
```

### GPUDispatcher (GPU-Accelerated)

**Use for:** ML inference, face detection, shader effects, image processing, GPU-accelerated encoding

**Characteristics:**
- Manages GPU execution contexts (CUDA streams, etc.)
- Minimizes CPU↔GPU memory transfers
- **Critical for realtime**: Keep data on GPU as long as possible
- Multiple actors can share GPU, scheduled via CUDA streams

**GPU Memory Transfer Patterns:**

1. **CPU → GPU → CPU** (BAD for realtime)
   ```python
   # Slow: Transfer every frame
   cpu_frame = await input.read()
   gpu_frame = torch.tensor(cpu_frame).cuda()  # CPU→GPU
   result = model(gpu_frame)
   cpu_result = result.cpu().numpy()  # GPU→CPU
   ```

2. **Keep on GPU** (GOOD for realtime)
   ```python
   # Fast: Stay on GPU across actors
   gpu_frame = await input.read()  # Already on GPU
   result = model(gpu_frame)  # GPU→GPU (fast)
   await output.send(result)  # Still on GPU
   ```

**Implementation:**

```python
class GPUDispatcher(Dispatcher):
    """
    Dispatcher for GPU-accelerated actors.

    Manages CUDA streams for concurrent GPU execution.
    Minimizes CPU↔GPU transfers by keeping data on device.
    """

    def __init__(self, device: str = 'cuda:0', streams: int = 4):
        self.device = torch.device(device)
        self.streams = [torch.cuda.Stream() for _ in range(streams)]
        self.stream_idx = 0

    async def schedule(self, actor: Actor, tick: TimedTick):
        """Execute on GPU with CUDA stream."""
        stream = self.streams[self.stream_idx]
        self.stream_idx = (self.stream_idx + 1) % len(self.streams)

        with torch.cuda.stream(stream):
            await actor.process(tick)

        # Don't block - allow overlap with CPU work
        # torch.cuda.synchronize() only if needed

    async def start(self):
        # Warm up GPU, allocate memory pools
        torch.cuda.set_device(self.device)

    async def stop(self):
        # Sync all streams before shutdown
        for stream in self.streams:
            stream.synchronize()

# Usage
face_detector = FaceDetectorActor(
    dispatcher=GPUDispatcher(device='cuda:0', streams=4),
    model='yolov8'
)

# Chain: GPU stays on GPU across actors
video_stream >> face_detector >> gpu_overlay >> gpu_encoder >> rtp_send
```

### GPU-Accelerated Pipeline Example

```python
class GPUVideoProcessingActor(Actor):
    """
    GPU-accelerated video processing actor.

    Keeps frames on GPU throughout pipeline:
    - Decode with NVDEC (GPU decoder)
    - Process with ML model (GPU inference)
    - Encode with NVENC (GPU encoder)
    """

    def __init__(self):
        super().__init__()
        self.inputs['video'] = StreamInput('video', VideoFrame)
        self.outputs['encoded'] = StreamOutput('encoded', bytes)

        # Use GPU dispatcher
        self.dispatcher = GPUDispatcher(device='cuda:0', streams=2)

        # Initialize GPU resources
        self.model = torch.jit.load('model.pt').cuda()
        self.decoder = NvDecoder()  # GPU decoder
        self.encoder = NvEncoder()  # GPU encoder

    async def process(self, tick: TimedTick):
        frame = await self.inputs['video'].try_read()
        if not frame:
            return

        # Decode on GPU (CPU→GPU transfer happens here)
        gpu_frame = self.decoder.decode(frame.data)  # Returns GPU tensor

        # ML inference on GPU (no transfer)
        with torch.no_grad():
            result = self.model(gpu_frame)  # GPU→GPU

        # Encode on GPU (no transfer)
        encoded = self.encoder.encode(result)  # GPU→GPU

        # Send encoded bytes (GPU→CPU transfer happens here)
        await self.outputs['encoded'].send(encoded.cpu())

# Usage
rtp_recv >> GPUVideoProcessingActor() >> rtp_send
```

### Shader-Based GPU Processing

For shader effects (OpenGL/Vulkan/Metal), actors can render directly to GPU textures:

```python
class ShaderEffectActor(Actor):
    """
    Apply GPU shader effects to video frames.

    Uses OpenGL/Vulkan for realtime effects.
    Keeps data on GPU as textures.
    """

    def __init__(self, shader_path: str):
        super().__init__()
        self.inputs['video'] = StreamInput('video', GPUTexture)
        self.outputs['video'] = StreamOutput('video', GPUTexture)

        self.dispatcher = GPUDispatcher(device='cuda:0')

        # Initialize OpenGL context and shader
        self.ctx = moderngl.create_context()
        self.shader = self.ctx.program(
            vertex_shader=load_shader(f"{shader_path}.vert"),
            fragment_shader=load_shader(f"{shader_path}.frag")
        )

    async def process(self, tick: TimedTick):
        texture = await self.inputs['video'].try_read()
        if not texture:
            return

        # Render shader effect (GPU→GPU, no CPU transfer)
        output_texture = self.shader.render(texture)

        await self.outputs['video'].send(output_texture)

# Usage - all GPU, no CPU transfers
video_stream >> ShaderEffectActor('bloom.glsl') >> ShaderEffectActor('vignette.glsl') >> display
```

### Realtime Event Processing

Events (keyboard, mouse, OSC, MIDI) are I/O-bound but may trigger GPU work:

```python
# I/O-bound event listener
osc_listener = OSCListenerActor()  # AsyncioDispatcher

# GPU-bound effect actor
shader_effect = ShaderEffectActor('distortion.glsl')  # GPUDispatcher

# Connect events to GPU work
osc_listener.outputs['params'] >> shader_effect.inputs['params']

# Video pipeline stays on GPU
video_stream >> shader_effect >> display
```

### Audio Processing

Audio is typically CPU-bound (DSP) but can be GPU-accelerated:

```python
# CPU-bound audio DSP
audio_mixer = AudioMixerActor(
    dispatcher=ThreadPoolDispatcher(workers=2)
)

# GPU-accelerated audio ML (e.g., source separation)
audio_ml = AudioSourceSeparationActor(
    dispatcher=GPUDispatcher(device='cuda:0')
)

# Chain
rtp_recv_audio >> audio_ml >> audio_mixer >> audio_output
```

### Dispatcher Selection Guidelines

| Workload | Dispatcher | Example Actors |
|----------|-----------|---------------|
| Network I/O | AsyncioDispatcher | RTPReceiveActor, RTPSendActor, DisplayActor |
| File I/O | AsyncioDispatcher | FileReadActor, FileSinkActor |
| Event handling | AsyncioDispatcher | KeyboardActor, MouseActor, OSCListenerActor |
| Video encoding | ThreadPoolDispatcher | H264EncoderActor (ffmpeg/PyAV) |
| Audio DSP | ThreadPoolDispatcher | AudioMixerActor, EQActor |
| Data transformation | ThreadPoolDispatcher | ResizeActor, ColorConvertActor |
| Heavy encoding | ProcessPoolDispatcher | H265EncoderActor (multi-pass) |
| ML inference | GPUDispatcher | FaceDetectorActor, PoseEstimationActor |
| Shader effects | GPUDispatcher | BloomActor, DistortionActor |
| GPU encoding | GPUDispatcher | NvencActor (NVIDIA GPU encoder) |

### Actor with Dispatcher

```python
class Actor(ABC):
    def __init__(self, parent: Optional['Actor'] = None, dispatcher: Optional[Dispatcher] = None):
        # ...
        self.dispatcher = dispatcher or AsyncioDispatcher()  # Default
        # ...

    async def _run(self):
        """Internal loop - uses dispatcher for scheduling."""
        try:
            await self.on_start()

            if not self.clock:
                self.clock = await self._acquire_clock()

            async for tick in self.clock.tick():
                if self.state != LifecycleState.RUNNING:
                    break

                # Schedule via dispatcher
                await self.dispatcher.schedule(self, tick)

            # ...
```

### Multi-Dispatcher Pipeline

```python
# I/O actors (AsyncioDispatcher)
rtp_recv = RTPReceiveActor()
display = DisplayActor()

# CPU actors (ThreadPoolDispatcher)
decoder = VideoDecoderActor(dispatcher=ThreadPoolDispatcher(workers=4))

# GPU actors (GPUDispatcher)
face_detect = FaceDetectorActor(dispatcher=GPUDispatcher(device='cuda:0'))
overlay = OverlayActor(dispatcher=GPUDispatcher(device='cuda:0'))

# Chain: automatic dispatcher transitions
rtp_recv >> decoder >> face_detect >> overlay >> display
#  I/O     →   CPU   →    GPU      →   GPU   →   I/O

# Efficient: Each actor runs on optimal executor
```

## Benefits

1. **Composable**: Like Unix pipes - any output >> any input
2. **Concurrent**: Actors run independently, no coordination needed
3. **Resilient**: Actor failures don't cascade (mailbox isolation)
4. **Scalable**: Thousands of lightweight actors
5. **Distributed**: Network-transparent, works locally or remotely
6. **Emergent**: Don't predict use cases, let them emerge
7. **AI-Orchestratable**: Agents can dynamically create and connect actors

## Implementation Notes

### Actor Spawning
**Anyone can spawn actors at any time.** Actors are not just created at initialization - they can be spawned:
- By application code at startup
- By other actors during processing
- Dynamically by agents generating code
- In response to events or messages

When spawned, actors immediately begin running. Internally, `__init__()` spawns `asyncio.create_task(self._run())`.

**Example - Actor spawning another actor:**
```python
class SupervisorActor(Actor):
    async def process(self, tick: TimedTick):
        # Spawn worker actor on demand
        if self.needs_worker():
            worker = WorkerActor()  # Spawns and runs immediately
            self.outputs['tasks'] >> worker.inputs['work']
```

### No Lifecycle Management
Actors are created running. No `start()`, `run()`, `stop()` methods visible to users. Once spawned, actors run until:
- They choose to stop themselves
- Their process completes (for finite actors)
- A supervisor actor terminates them

### Automatic Clock Sync
Actors automatically sync to upstream clocks by reading clock metadata from first received message. No manual sync needed.

### Realtime Message Dropping (Not Backpressure)
**For realtime systems, messages are DROPPED when mailboxes fill up, not queued.**

If a downstream actor's mailbox is full:
- Upstream `send()` drops the message (does NOT block)
- This prevents upstream actors from stalling
- Old frames/data are discarded in favor of new

**Why:** In realtime (video, audio, events), old data is worthless. Better to drop old frames than block the pipeline.

**If buffering is needed:** Place an explicit `BufferActor` between upstream and downstream:
```python
upstream >> BufferActor(capacity=30) >> slow_downstream
```

This is similar to GStreamer's queue elements but with explicit control.

### Error Handling
Actor exceptions are contained. Other actors continue running. Supervisor actors can monitor and restart failed actors (future).

## Performance Profiling & Testing

### Design Philosophy

**Python implementation with Rust migration path:**

- **Phase 3**: Pure Python implementation
  - NumPy arrays (zero-copy, C backend)
  - PyAV (FFmpeg C library)
  - Skia/Cairo (C++ rendering)
  - PyTorch/CUDA (GPU work in C++/CUDA)
  - Actors in separate processes (bypass GIL)

- **If bottlenecks found**: Migrate actor runtime to Rust (PyO3)
  - Keep Python API (like Pydantic V2, Polars)
  - Rust core (ring buffers, RTP stack, actor runtime)
  - Python wrapper for business logic

### Opt-In Profiling System

**Zero overhead when disabled:**

```python
# Enable profiling via environment variable
STREAMLIB_PROFILE=1 python demo.py

# Disabled (default): No measurements, no overhead
python demo.py
```

**Profiling points:**

```python
from streamlib.profiling import profile

class VideoActor:
    async def process(self, frame):
        with profile('actor.process', actor_id=self.id, frame_number=frame.num):
            # Profile specific operations
            with profile('actor.decode'):
                decoded = await self.decode(frame)

            with profile('actor.ml_inference'):
                result = await self.model.infer(decoded)

            return result
```

**Built-in profiling for core components:**
- `compositor.composite` - Total composition time
- `compositor.blend` - Alpha blending
- `layer.{name}.draw` - Per-layer rendering
- `actor.{name}.process` - Actor processing
- `network.send/receive` - Network I/O

**Output format:**

```
=== Profiling Results ===

Operation                   Count      Mean      P50      P95      P99     Max
------------------------------------------------------------------------------
frame.total                  1000   16.2ms   16.1ms   17.5ms   18.2ms  19.1ms
frame.decode                 1000    5.1ms    5.1ms    5.5ms    5.8ms   6.2ms
frame.process                1000    8.5ms    8.4ms    9.1ms    9.5ms  10.1ms
compositor.composite         1000   14.6ms   14.5ms   15.2ms   15.7ms  16.2ms
```

### Performance Targets

**Frame latency:**

| Resolution | Target | Acceptable | Too Slow |
|-----------|--------|------------|----------|
| 1080p60   | < 16ms | < 20ms     | > 20ms   |
| 1080p30   | < 33ms | < 40ms     | > 40ms   |
| 4K30      | < 33ms | < 40ms     | > 40ms   |
| 4K60      | < 16ms | < 20ms     | > 20ms   |

**Jitter target:** < 1ms variance (P99 - P50)

**Why these targets:**
- Frame time = 1000ms / fps
- Need some headroom for system variance
- Jitter causes visible stutter

### What to Profile

1. **Frame processing latency** - End-to-end time per frame
2. **Jitter** - Variance in frame times (P99 - P50)
3. **Per-actor overhead** - CPU/memory per actor instance
4. **Network latency** - RTP packet send/receive times
5. **Memory copies** - Ring buffer should be zero-copy
6. **GPU transfer** - CPU→GPU and GPU→CPU times

### When to Migrate to Rust

**Python works for:**
- Up to 4K30 on modern hardware
- < 10 concurrent actors
- NumPy/PyAV/CUDA doing heavy lifting

**Migrate to Rust if:**
- Frame latency > 1 frame time
- Jitter > 2ms consistently
- Actor overhead > 5% CPU per actor
- Profiling shows Python as bottleneck (not NumPy/FFmpeg)

**Hybrid approach** (like Pydantic V2):
- Python API (actor definitions, business logic, AI orchestration)
- Rust core (runtime, ring buffers, RTP stack, timing)
- PyO3 bindings for zero-copy data transfer
- Best of both worlds: AI-friendly API + realtime performance

## Philosophy Checks

Before implementing any feature, ask:

1. ✅ Is each component an independent actor?
2. ✅ Does it run continuously from creation?
3. ✅ Does it communicate only via messages?
4. ✅ Is state fully encapsulated?
5. ✅ Can agents dynamically create and connect it?
6. ✅ Does it work the same locally and over network?
7. ✅ Is it like `grep | sed | awk` for streams?

If any answer is "no", the design is wrong.

## References

- Actor Model: https://en.wikipedia.org/wiki/Actor_model
- Akka (Actor framework): https://doc.akka.io/
- Erlang OTP: https://www.erlang.org/
- WebRTC: https://webrtc.org/
- Unix Philosophy: https://en.wikipedia.org/wiki/Unix_philosophy
