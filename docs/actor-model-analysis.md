# Actor Model Analysis for streamlib

## Overview

This document captures our design decisions for streamlib's actor-based architecture, based on research of Akka, Erlang/OTP, and Ergo frameworks, adapted for realtime streaming.

## Core Design Decisions

### 1. Data Flow & Message Semantics

**Our Decision: Ring buffers with latest-read semantics**

**Why ring buffers, not queues:**
- **Professional broadcast standard** - SMPTE ST 2110 uses 2-3 frame ring buffers
- **Fixed memory** - No growing queues, bounded latency
- **Zero-copy** - GPU buffers stay on GPU (no CPU↔GPU transfers)
- **Latest-read** - Old frames overwritten, always process latest
- **Realtime-appropriate** - Old data is worthless

**Implementation:**
```python
class RingBuffer:
    """3-slot circular buffer (matches broadcast practice)."""
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

**At-most-once delivery:**
- Frames may be **overwritten** before read (not duplicated)
- **No retries**, no delivery guarantees
- Better to skip old frames than process stale data

**This matches:**
- GStreamer behavior (drop frames when downstream slow)
- WebRTC behavior (skip old frames for realtime)
- SMPTE ST 2110 practice (bounded ring buffers)

**Alternative:** If buffering needed for specific use cases, add explicit `BufferActor` with larger ring buffer between stages.

### 2. Timestamps vs Ordering

**Our Decision: NO ordering guarantee, timestamps for temporal order**

- Data in ring buffers has **no ordering** (just latest available)
- All data carries **PTP timestamps** for temporal information
- **Timestamps = metadata** (recording), not sync enforcement
- Receivers can reorder using timestamps **if needed**

**Why:**
- Standard Actor model guarantees FIFO per sender-receiver pair
- **We intentionally reject this** for realtime streaming
- Ring buffers = latest-read, not ordered queue
- Matches GStreamer, WebRTC, SMPTE 2110 behavior
- Allows UDP, multi-path routing, packet loss recovery
- **Delivery order ≠ temporal order**

**Implementation:**
- All frames carry `timestamp` (PTP time when created)
- Ring buffer provides latest available, not oldest
- Actor reads latest, timestamps it with current tick time
- Network boundary: RTP packets carry PTP timestamp in header

**Example:**
```python
async def process(self, tick: TimedTick):
    # Read latest from ring buffer (may be from tick-5)
    frame = self.inputs['video'].read_latest()

    # Process
    result = self.process_frame(frame)

    # Timestamp with current tick
    result.timestamp = tick.timestamp  # Current PTP time

    # Write to ring buffer
    self.outputs['video'].write(result)
```

**This is a departure from standard Actor model, but correct for realtime streaming.**

### 3. Supervision & Lifecycle Management

**Our Decision: Supervision is OPT-IN, not mandatory**

**Philosophy:**
- **Actors are atomic units** - If they fail, that's on them
- **Supervision is optional** - Parent chooses whether to add supervisor
- **Simple stays simple** - Don't force supervision on every actor
- **AI-friendly** - Explicit reasoning: "Do I need supervision here?"

**Why:**
- Standard Actor model (Erlang/Akka) mandates supervision trees
- **We reject this** - too complex for simple use cases
- AI agents should explicitly reason about supervision needs
- Most actors don't need automatic restart (e.g., network receivers)

**Required: Lifecycle States**
- `RUNNING` - Actor is processing messages
- `STOPPED` - Actor stopped gracefully
- `FAILED` - Actor encountered unrecoverable error
- Observable by parent or external actors

**Required: Parent-Child Relationships**
- Parent spawns children with `spawn()`
- **Parent death → children stop automatically**
- Children register with parent
- Clean cascading shutdown

**Optional: Supervision**
- Supervisor is **created on parent** if needed
- Parent **chooses** to use supervisor when spawning
- Supervisor monitors child lifecycle
- Supervisor decides: restart, stop, ignore

**Example:**
```python
class ParentActor(Actor):
    def __init__(self):
        super().__init__()
        # Optional supervisor (only if you want restart behavior)
        self.supervisor = Supervisor(
            strategy=RestartStrategy(max_retries=3)
        )

    async def process(self, tick: TimedTick):
        # Without supervision (child fails = stays failed)
        simple_child = self.spawn(WorkerActor)

        # With supervision (child fails = supervisor restarts it)
        supervised_child = self.spawn(
            WorkerActor,
            supervisor=self.supervisor
        )
```

**Implementation Status:** Designed, needs Phase 3 implementation

### 4. Network Transparency (SMPTE ST 2110)

**Our Decision: SMPTE ST 2110 for broadcast interoperability**

**Why SMPTE 2110:**
- **Professional broadcast standard** (not WebRTC, not NDI)
- **Separate streams** for video, audio, data (not multiplexed)
- **RTP over UDP** - low latency, no connection overhead
- **PTP timestamps** - microsecond synchronization (IEEE 1588)
- **Interoperable** with real broadcast equipment
- **Embeddable** on edge devices (agent chips)

**Architecture:**
- Same `>>` operator works locally and remotely
- Manual IP:port addressing (AI agents configure directly)
- Minimal core, no discovery/registry (agents handle addressing)

**OSI Model Alignment:**
- **Layer 4**: UDP (connectionless, low latency)
- **Layer 5**: RTP (Real-time Transport Protocol, RFC 3550)
- **Layer 6**: SMPTE 2110 payload formats
- **Layer 7**: Actor system (our innovation)

**SMPTE 2110 Key Principle: Separate Streams**

Video and audio are **separate actors**, **separate RTP streams**, running **concurrently**:

```python
# Video actor (separate stream, ST 2110-20)
video_gen >> VideoRTPSendActor('192.168.1.100', 5004)
VideoRTPReceiveActor(5004) >> display

# Audio actor (separate stream, ST 2110-30)
audio_gen >> AudioRTPSendActor('192.168.1.100', 5006)
AudioRTPReceiveActor(5006) >> speaker

# Both actors run concurrently, synchronized by PTP timestamps
```

**Why separate actors?**
- Matches SMPTE 2110 architecture (separate essence streams)
- Video can use GPUDispatcher, audio can use ThreadPoolDispatcher
- Independent processing, concurrent execution
- Receivers sync using PTP timestamps

**Network Actor Architecture: Jitter Buffer + Ring Buffer**

RTP network actors use **two-stage buffering** (matches professional broadcast):

```python
class VideoRTPReceiveActor(Actor):
    def __init__(self, port: int):
        # Stage 1: Jitter buffer (reorder RTP packets)
        self.jitter_buffer = JitterBuffer(max_delay_ms=10)  # Bounded

        # Stage 2: Ring buffer (hold frames)
        self.frame_buffer = RingBuffer(slots=3)

    async def process(self, tick: TimedTick):
        # Receive RTP packets (may arrive out of order over UDP)
        packets = self.receive_udp_packets()

        # Reorder in jitter buffer (small, bounded)
        frame = self.jitter_buffer.reassemble(packets)

        if frame:
            # Frame has PTP timestamp from sender
            # Write to ring buffer for downstream actors
            self.frame_buffer.write(frame)
```

**Why jitter buffer + ring buffer:**
- **Jitter buffer** (1-10ms): Handle network packet reordering (UDP packets arrive out of order)
- **Ring buffer** (2-3 frames): Provide latest frame to downstream actors
- **Bounded memory**: Both buffers have fixed size (realtime requirement)
- **Matches SMPTE ST 2110**: Standard practice in professional broadcast

**Send side:**
```python
class VideoRTPSendActor(Actor):
    def __init__(self, host: str, port: int, ptp_clock: PTPClock):
        self.socket = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        self.dest = (host, port)
        self.ptp_clock = ptp_clock

    async def process(self, tick: TimedTick):
        # Read latest frame from ring buffer
        frame = self.inputs['video'].read_latest()

        # Encode to SMPTE 2110-20 RTP packets
        rtp_packets = encode_smpte_2110_video(
            frame=frame,
            timestamp=self.ptp_clock.now(),  # PTP timestamp
            sequence=self.sequence
        )

        # Send via UDP
        for packet in rtp_packets:
            self.socket.sendto(packet, self.dest)
```

**Clock sources:**
- Local actors: Use `GenlockClock` (SDI) or `SoftwareClock` (bathtub)
- Network actors: Sync to network's `PTPClock` (IEEE 1588 grandmaster)
- Hybrid device: Can switch between genlock (SDI) and PTP (network)

**Implementation Status:** Designed, Phase 4 implementation

### 5. Timing & Concurrent Execution

**Our Decision: Tick-based processing with swappable clock sources**

## Clock Abstraction

**Actors receive ticks from clock, don't care about source:**

```python
class Clock(ABC):
    """Abstract clock source - PTP, genlock, or software."""
    @abstractmethod
    async def tick(self) -> AsyncIterator[TimedTick]:
        """Yield ticks with timestamps."""
        pass

class PTPClock(Clock):
    """IEEE 1588 PTP for SMPTE 2110 networks."""
    def __init__(self, ptp_client, fps=60):
        self.ptp_client = ptp_client  # Syncs to PTP grandmaster
        self.fps = fps

    async def tick(self):
        while True:
            timestamp = self.ptp_client.now()  # Microsecond-accurate
            yield TimedTick(timestamp=timestamp, source='ptp')
            await asyncio.sleep(1/self.fps)

class GenlockClock(Clock):
    """Hardware genlock for SDI devices."""
    def __init__(self, genlock_device):
        self.device = genlock_device  # Hardware sync input

    async def tick(self):
        while True:
            await self.device.wait_for_pulse()  # Block until hardware pulse
            timestamp = time.time()
            yield TimedTick(timestamp=timestamp, source='genlock')

class SoftwareClock(Clock):
    """Software clock for bathtub mode (no external sync)."""
    def __init__(self, fps=60):
        self.fps = fps

    async def tick(self):
        while True:
            timestamp = time.time()
            yield TimedTick(timestamp=timestamp, source='software')
            await asyncio.sleep(1/self.fps)
```

**Why swappable:**
- Device with SDI + network: Use `GenlockClock` or `PTPClock` (configurable)
- Bathtub mode: Use `SoftwareClock`
- Runtime switching: If genlock signal lost, fall back to PTP
- Actors don't care: Same `async for tick in clock.tick()` interface

**Example: Hybrid device**
```python
# Device with SDI and network ports
if genlock_signal_present:
    clock = GenlockClock(sdi_port)  # Sync to SDI genlock
elif ptp_available:
    clock = PTPClock(ptp_client)    # Sync to network PTP
else:
    clock = SoftwareClock(fps=60)   # Free-run in software
```

## Data Flow: Ring Buffers (Not Queues)

**Our Decision: Bounded ring buffers for zero-copy realtime**

**Why ring buffers:**
- Professional broadcast uses 2-3 frame ring buffers
- Fixed memory (no growing queues)
- Zero-copy for GPU data
- Latest-read semantics (old frames discarded)
- Aligns with SMPTE ST 2110 practice

**Implementation:**
```python
class RingBuffer:
    """Fixed-size circular buffer for frames."""
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

**For GPU (zero-copy):**
```python
class GPURingBuffer:
    """GPU memory slots, no CPU transfer."""
    def __init__(self, slots=3, shape=(1920, 1080, 3)):
        # Pre-allocate GPU buffers
        self.buffers = [
            torch.zeros(shape, device='cuda') for _ in range(slots)
        ]
        self.write_idx = 0

    def get_write_buffer(self):
        """Get GPU buffer to write into."""
        return self.buffers[self.write_idx]

    def advance(self):
        """Mark current buffer as ready."""
        self.write_idx = (self.write_idx + 1) % len(self.buffers)

    def get_read_buffer(self):
        """Get latest GPU buffer (zero-copy)."""
        idx = (self.write_idx - 1) % len(self.buffers)
        return self.buffers[idx]
```

## Actor Processing Model

**Tick = Signal to process, not data carrier:**

```python
async def process(self, tick: TimedTick):
    # Tick says "it's time T, do work"

    # Read LATEST from ring buffer (not queued data)
    frame = self.inputs['video'].read_latest()

    # Do work (may take longer than tick interval)
    result = self.process_frame(frame)

    # Write to ring buffer slot (overwrites old)
    result.timestamp = tick.timestamp  # "This result is for time T"
    self.outputs['video'].write(result)
```

**If tick arrives while actor is busy:**
- Tick is **dropped** (ignored)
- Actor continues working
- Next tick after finishing: processes with latest buffer data

**Sequential = One tick at a time:**
- Actor processes tick 0, then tick 5, then tick 10... (skips ticks while busy)
- Does NOT mean "messages in FIFO order"
- Just means: handle one tick at a time, don't interleave

## Concurrent Execution: Dispatchers

**Multiple actors run concurrently, each on appropriate executor:**

**AsyncioDispatcher (I/O-bound):**
- Network I/O, file I/O, event handling
- Single event loop, thousands of actors
- **Examples:** RTPReceiveActor, DisplayActor, OSCListenerActor

**ThreadPoolDispatcher (CPU-bound):**
- Video encoding, audio DSP, data transformation
- Python threads (4-8 workers)
- Good for C extensions (numpy, OpenCV, PyAV bypass GIL)
- **Examples:** H264EncoderActor, AudioMixerActor

**ProcessPoolDispatcher (Heavy CPU):**
- Multi-pass encoding, batch processing
- Multiple processes (2-4 workers)
- Bypasses GIL, true parallelism
- **Examples:** H265EncoderActor (multi-pass)

**GPUDispatcher (GPU-accelerated):**
- ML inference, face detection, shader effects, GPU encoding
- CUDA streams for concurrent GPU execution
- Keeps data on GPU (minimal CPU↔GPU transfers)
- **Examples:** FaceDetectorActor, ShaderEffectActor, NvencActor

**Separate actors run concurrently (true parallelism):**
```python
# Tick arrives at all actors simultaneously
# Each actor processes on its own dispatcher IN PARALLEL

# Video actor - GPU-bound processing
video_actor = VideoProcessorActor(
    dispatcher=GPUDispatcher(device='cuda:0')
)

# Audio actor - CPU-bound DSP
audio_actor = AudioDSPActor(
    dispatcher=ThreadPoolDispatcher(workers=4)
)

# When tick arrives:
# - Video actor starts GPU work (8ms)
# - Audio actor starts CPU work (1ms)
# - Both happening SIMULTANEOUSLY
# - Total latency: max(8ms, 1ms) = 8ms (not 8+1=9ms)
```

**Why this matters for broadcast quality realtime:**
- **Maximum parallelism** - Audio, video, events all processing concurrently
- **Optimal resource usage** - GPU for video, CPU for audio, both utilized
- **Low latency** - Parallel work completes in max(work_times), not sum(work_times)
- **Scalability** - Add more actors without blocking existing ones

## Dispatcher Selection for Audio

**CPU (ThreadPoolDispatcher) - Typical for traditional audio DSP:**
- **Filters, EQ, compression, reverb** - Low latency (< 1ms per effect)
- **Mixing, routing** - Small block sizes (64-512 samples)
- **Professional plugins** - Waves, FabFilter, Soundtoys style processing
- **Latency requirement** - < 10ms for realtime audio

```python
audio_dsp = AudioDSPActor(
    dispatcher=ThreadPoolDispatcher(workers=4)
)

async def process(self, tick: TimedTick):
    audio = self.inputs['audio'].read_latest()  # 512 samples

    # Traditional DSP chain (all CPU)
    audio = self.high_pass_filter(audio)    # 0.1ms
    audio = self.eq(audio)                  # 0.2ms
    audio = self.compressor(audio)          # 0.1ms
    audio = self.reverb(audio)              # 0.5ms
    # Total: ~1ms (excellent for realtime)

    self.outputs['audio'].write(audio)
```

**GPU (GPUDispatcher) - For audio ML models:**
- **Speech synthesis** - TTS models (Tacotron, VITS)
- **Source separation** - Vocal/instrument isolation
- **Noise reduction** - ML-based denoising
- **Voice conversion** - Real-time voice changing
- **Higher latency** - 10-100ms (acceptable for ML quality)

```python
speech_gen = SpeechSynthesisActor(
    dispatcher=GPUDispatcher(device='cuda:0')
)

async def process(self, tick: TimedTick):
    text = self.inputs['text'].read_latest()

    # ML model on GPU (higher latency but ML-quality)
    audio = self.tts_model.generate(text)  # 50ms

    self.outputs['audio'].write(audio)
```

**Caveat: Not a hard limitation**
- Use GPU for audio **when latency permits** and ML quality needed
- Traditional DSP on CPU **for lowest latency**
- Choice depends on use case: realtime mixing (CPU) vs speech generation (GPU)
- Both can run in parallel (audio DSP on CPU + speech gen on GPU simultaneously)

## SMPTE ST 2110 Network Boundary

**Network actors use jitter buffer + ring buffer:**

```python
class RTPReceiveActor(Actor):
    def __init__(self, port: int):
        # Jitter buffer: reorder RTP packets (10ms max)
        self.jitter_buffer = JitterBuffer(max_delay_ms=10)

        # Ring buffer: hold frames (3 slots)
        self.frame_buffer = RingBuffer(slots=3)

    async def process(self, tick: TimedTick):
        # Receive RTP packets (may arrive out of order)
        packets = self.receive_rtp_packets()

        # Reorder in jitter buffer
        frame = self.jitter_buffer.add_packets(packets)

        if frame:
            # Frame already has PTP timestamp from sender
            # Write to ring buffer
            self.frame_buffer.write(frame)
```

**Why this matches SMPTE 2110:**
- Small bounded jitter buffer (1-10ms) for packet reordering
- Ring buffer (2-3 frames) for latest-read
- PTP timestamps in RTP headers (IEEE 1588)
- No growing queues, fixed memory

## Implementation Status

✅ **Designed:**
- Clock abstraction (PTP, genlock, software)
- Ring buffers (CPU and GPU)
- Dispatcher abstraction (Asyncio, ThreadPool, ProcessPool, GPU)
- SMPTE 2110 jitter + ring buffer model

**Phase 3 Implementation:**
- Lifecycle management
- Ring buffer implementation
- Dispatcher implementation
- Clock abstraction implementation

### 6. Actor References & Addressing

**Our Decision: URI-based addressing with port-per-output**

Inspired by Cloudflare Durable Objects: actors referenced by URIs, automatically created on demand.

## URI Schema

```
actor://host/ActorClass/instance-id
```

**Examples:**
- `actor://192.168.1.100/VideoCapture/cam-1`
- `actor://edge-device/StereoCam/stereo-1`
- `actor://gpu-server/FaceDetect/proc-1`
- `actor://local/Display/main`

## Control Plane vs Data Plane

**Layer 7 (Control Plane): Actor URIs**
- Actor lifecycle (get/create/destroy)
- Control messages (configure, start, stop)
- Registry lookup (URI → network endpoints)
- Location transparency (local stub vs remote stub)

**Layers 4-6 (Data Plane): SMPTE ST 2110 RTP/UDP**
- Media streams (video, audio, data)
- Each actor **output** gets unique UDP port
- RTP over UDP, PTP timestamps
- Interoperable with real SMPTE equipment

## Port Allocation: One Port Per Output

**SMPTE ST 2110 principle: Each stream = unique UDP port**

```python
# Get or create actor by URI
camera = get_actor('actor://edge/VideoCapture/cam-1')

# Actor has output(s), each gets unique UDP port
# Registry maps: actor-id.output-name → host:port
# cam-1.video → edge:5004
```

**Stereo vision example (multiple outputs):**
```python
stereo_cam = get_actor('actor://edge/StereoCam/stereo-1')

# Actor has multiple outputs:
stereo_cam.outputs['left_video']   # → edge:5004
stereo_cam.outputs['right_video']  # → edge:5006
stereo_cam.outputs['audio']        # → edge:5008

# Each output = separate SMPTE ST 2110 RTP stream = unique UDP port
```

## Registry Structure

**Maps actor outputs to network endpoints:**
```python
{
    'stereo-1.left_video': 'edge:5004',
    'stereo-1.right_video': 'edge:5006',
    'stereo-1.audio': 'edge:5008',
    'proc-1.left': 'gpu:5020',
    'proc-1.right': 'gpu:5022',
    'proc-1.depth': 'gpu:5024',
    'display-1.video': 'local:5030'
}
```

## Connection Flow

```python
# 1. Get/create actors (allocates UDP ports)
camera = get_actor('actor://edge/VideoCapture/cam-1')
# Creates actor, binds output to UDP edge:5004
# Registry: cam-1.video → edge:5004

processor = get_actor('actor://gpu/FaceDetect/proc-1')
# Creates actor, binds input to UDP gpu:5010
# Registry: proc-1.video → gpu:5010

# 2. Connect actors (sets up RTP streams)
camera >> processor

# Behind the scenes:
# - Query registry: cam-1.video → edge:5004 (sender)
# - Query registry: proc-1.video → gpu:5010 (receiver)
# - Configure camera to send SMPTE ST 2110 RTP to gpu:5010
# - Configure processor to receive SMPTE ST 2110 RTP on port 5010
# - Media flows: edge:5004 → RTP/UDP → gpu:5010
```

## Location Transparency

**Local actors:**
```python
# Get local actor (same process)
display = get_actor('actor://local/Display/main')
# Returns: Direct Python object reference
# No network overhead for control messages
```

**Remote actors:**
```python
# Get remote actor (different host)
camera = get_actor('actor://edge/VideoCapture/cam-1')
# Returns: Remote stub (proxy object)
# Control messages sent over network
# Media streams still use SMPTE ST 2110 RTP/UDP
```

**Same API, different implementation:**
```python
# Works identically for local and remote
actor.send_control_message({'action': 'set_fps', 'fps': 60})

# Local: Direct method call
# Remote: Send control message over network (TCP/HTTP)
```

## SMPTE Interoperability

**Connect to real SMPTE equipment (no URIs, just IP:port):**

```python
# Our actor (with URI)
processor = get_actor('actor://gpu/FaceDetect/proc-1')
# Bound to: gpu:5010

# Connect to real SMPTE camera (no URI, just IP:port)
processor.inputs['video'].receive_from('192.168.1.50:5004')
# Processor receives SMPTE ST 2110 RTP from real camera

# Send to real SMPTE display
processor.outputs['video'].send_to('192.168.1.60:5006')
# Real display receives SMPTE ST 2110 RTP from processor
```

**Real SMPTE equipment example:**
```
Professional 4K Camera (192.168.1.50)
├── Video output    → UDP 192.168.1.50:5004 (ST 2110-20)
├── Audio output    → UDP 192.168.1.50:5006 (ST 2110-30)
└── Metadata output → UDP 192.168.1.50:5008 (ST 2110-40)

↓ (SMPTE ST 2110 RTP/UDP)

Our Actor (actor://gpu/Processor/proc-1)
├── Video input     → UDP gpu:5010 (receives from 192.168.1.50:5004)
├── Audio input     → UDP gpu:5012 (receives from 192.168.1.50:5006)
└── Video output    → UDP gpu:5014 (sends to real display)
```

## Port Allocation Strategies

**Option 1: Dynamic (OS assigns random available port)**
```python
camera = get_actor('actor://edge/VideoCapture/cam-1')
# OS picks: 50234
# Registry: cam-1.video → edge:50234
```

**Option 2: Sequential (predictable, SMPTE-friendly)**
```python
# First VideoCapture → port 5004
actor://edge/VideoCapture/cam-1  →  edge:5004

# Second VideoCapture → port 5006 (even numbers, ST 2110 convention)
actor://edge/VideoCapture/cam-2  →  edge:5006

# First AudioCapture → port 5008
actor://edge/AudioCapture/mic-1  →  edge:5008
```

**For broadcast interoperability: Use sequential/predictable ports (SMPTE convention)**

## Actor Lifecycle

**Get or create:**
```python
# First call: Creates actor, allocates ports, registers
actor = get_actor('actor://host/ActorClass/instance-1')

# Subsequent calls: Returns existing actor
actor2 = get_actor('actor://host/ActorClass/instance-1')
assert actor is actor2  # Same instance
```

**Multiple instances:**
```python
# Different instance IDs = different actors
cam1 = get_actor('actor://edge/VideoCapture/cam-1')  # edge:5004
cam2 = get_actor('actor://edge/VideoCapture/cam-2')  # edge:5006
# Two separate actors, two separate UDP ports
```

## Implementation Status

**Phase 4 Implementation:**
1. URI parser: `actor://host/ActorClass/instance-id`
2. Actor registry: URI → actor reference, output → UDP port
3. Port allocator: Assign UDP ports per output
4. Local/remote stubs: Direct reference vs network proxy
5. RTP stream configuration: Map actor connections to SMPTE streams
6. SMPTE interop: Manual IP:port connections for real equipment

**Benefits:**
- ✅ Discovery: Find actors without manual IP management
- ✅ Lifecycle: Actors created on demand (Durable Objects-style)
- ✅ Location transparency: Same API local vs remote
- ✅ SMPTE interop: Media flows via standard RTP/UDP
- ✅ AI-friendly: Agents reason about URIs, not low-level network config

## What We Got Right

✅ **Message passing** - Mailboxes, async send/receive
✅ **Encapsulation** - Private state, no shared memory
✅ **At-most-once delivery** - Correct for realtime
✅ **Composability** - Pipe operator, dynamic connections
✅ **Auto-start** - Actors run immediately
✅ **Message dropping** - Realtime-appropriate backpressure

## Phase 3 Implementation Needs

### Core Infrastructure

**What we need to implement:**

1. **Ring Buffers**
   - Fixed-size circular buffers (3 slots)
   - CPU ring buffers for general data
   - GPU ring buffers for zero-copy video/image data
   - Latest-read semantics (overwrite old)

2. **Dispatchers**
   - AsyncioDispatcher (I/O-bound, default)
   - ThreadPoolDispatcher (CPU-bound)
   - ProcessPoolDispatcher (heavy compute)
   - GPUDispatcher (GPU-accelerated, CUDA streams)

3. **Clock Abstraction**
   - PTPClock (IEEE 1588, SMPTE 2110 networks)
   - GenlockClock (SDI hardware sync)
   - SoftwareClock (bathtub mode)
   - Swappable clock sources

4. **Actor Registry**
   - URI parser: `actor://host/ActorClass/instance-id`
   - Registry: URI → actor reference
   - Port allocator: UDP port per output
   - Local/remote stub handling

**What we're punting to Phase 4+:**
- Lifecycle management (RUNNING/STOPPED/FAILED states)
- Parent-child relationships and cleanup
- Supervision and failure recovery
- Complex actor coordination

**Why punt lifecycle?**
- Not needed for Phase 3: Actors just run until program exits
- Python garbage collection handles cleanup
- Registry tracks existence implicitly
- Supervision is opt-in (can add later)

## Recommendations

### Immediate (Phase 3)

1. **Implement lifecycle management**
   - Lifecycle states (RUNNING, STOPPED, FAILED)
   - Parent-child relationships (spawn, track children)
   - Parent death → children stop automatically
   - Optional Supervisor class (not mandatory)

2. **Implement dispatcher system** ✅ **DESIGNED**
   - AsyncioDispatcher (I/O-bound, default)
   - ThreadPoolDispatcher (CPU-bound)
   - ProcessPoolDispatcher (heavy compute)
   - GPUDispatcher (GPU-accelerated, CUDA streams, shaders)

3. **Documentation complete** ✅
   - Realtime semantics documented (NO ordering, at-most-once)
   - Message dropping behavior documented
   - Timestamps for receiver-side ordering documented

### Phase 4 (Network - SMPTE 2110 Core)

1. **RTP/UDP transport**
   - Implement RFC 3550 (RTP)
   - UDP sockets for send/receive
   - Manual IP:port addressing

2. **SMPTE 2110 payload formats**
   - ST 2110-20: Uncompressed video
   - ST 2110-22: Compressed video (H.264, JPEG XS)
   - ST 2110-30: PCM audio
   - ST 2110-40: Ancillary data

3. **RTPSendActor / RTPReceiveActor**
   - Minimal, embeddable actors
   - Encode/decode SMPTE packets
   - PTP timestamps in headers

4. **Interoperability testing**
   - Test with real SMPTE equipment
   - Verify compatibility with broadcast infrastructure

### Future Enhancements

1. **Persistence** - Actor state snapshots for recovery
2. **Clustering** - Multi-node coordination (beyond SMPTE 2110)

## Concurrent Execution Strategy

**Current:** Single asyncio event loop (one thread)

**Fully Designed Multi-Model** ✅ (see `architecture.md`):

1. **I/O-Bound Actors** (network streams, file I/O, events)
   - Use `AsyncioDispatcher` (default)
   - Thousands of actors on one thread
   - Efficient for I/O wait
   - **Examples:** RTPReceiveActor, DisplayActor, OSCListenerActor

2. **CPU-Bound Actors** (encoding, traditional audio DSP, data transformation)
   - Use `ThreadPoolDispatcher`
   - Python threads (GIL limitations)
   - Good for C extensions (numpy, OpenCV, PyAV)
   - **Video:** H264EncoderActor, ColorConvertActor
   - **Audio:** AudioMixerActor, EQActor, CompressorActor (traditional DSP)
   - **Latency:** 1-10ms (excellent for realtime)

3. **Heavy Compute Actors** (multi-pass encoding, complex processing)
   - Use `ProcessPoolDispatcher`
   - Multiple processes (bypass GIL)
   - Higher overhead but true parallelism
   - **Examples:** H265EncoderActor (multi-pass)

4. **GPU-Accelerated Actors** (ML inference, shaders, GPU encoding, audio ML)
   - Use `GPUDispatcher`
   - CUDA streams for concurrent execution
   - Minimizes CPU↔GPU transfers (keep data on GPU)
   - Shader support (OpenGL/Vulkan/Metal)
   - **Video:** FaceDetectorActor, ShaderEffectActor, NvencActor
   - **Audio ML:** SpeechSynthesisActor, SourceSeparationActor, VoiceConversionActor
   - **Latency:** 10-100ms (acceptable for ML quality)
   - **Note:** Not a hard limitation - use GPU for audio when latency permits and ML quality needed

**Example (parallel video + audio processing):**
```python
# I/O-bound: async event loop
video_recv = RTPReceiveActor()  # AsyncioDispatcher
audio_recv = RTPReceiveActor()  # AsyncioDispatcher

# CPU-bound: thread pool (video decoding + traditional audio DSP)
video_decoder = VideoDecoderActor(
    dispatcher=ThreadPoolDispatcher(workers=4)
)
audio_dsp = AudioMixerActor(
    dispatcher=ThreadPoolDispatcher(workers=2)
)

# GPU-bound: CUDA streams (video ML + audio ML)
face_detect = FaceDetectorActor(
    dispatcher=GPUDispatcher(device='cuda:0', streams=4)
)
speech_gen = SpeechSynthesisActor(
    dispatcher=GPUDispatcher(device='cuda:0', streams=2)
)

# Video path: I/O → CPU → GPU → I/O
video_recv >> video_decoder >> face_detect >> display

# Audio DSP path: I/O → CPU → I/O (low latency)
audio_recv >> audio_dsp >> speaker

# Audio ML path: I/O → GPU → I/O (higher latency, ML quality)
text_events >> speech_gen >> speaker

# All actors run IN PARALLEL:
# - video_decoder on CPU threads
# - audio_dsp on CPU threads (different pool)
# - face_detect on GPU
# - speech_gen on GPU (concurrent via CUDA streams)
```

## Realtime Considerations

For realtime systems (video, audio, events), we prioritize:

1. **Low latency** over reliability
   - Overwrite old frames in ring buffer rather than queue
   - At-most-once delivery
   - Skip ticks when actor busy (drop old work)

2. **Latest data** over ordering
   - Ring buffers provide latest available (not oldest)
   - No FIFO ordering - timestamps provide temporal information
   - Can tolerate out-of-order delivery (UDP networks)

3. **Availability** over consistency
   - Keep processing even with failures
   - Optional supervision for recovery
   - Actors continue running independently

4. **Bounded resources** over completeness
   - Fixed-size ring buffers (2-3 slots for frames)
   - Small jitter buffers (1-10ms for network packet reordering)
   - Explicit larger buffering when needed (BufferActor)

5. **Parallelism** over sequential processing
   - Multiple actors run concurrently on optimal dispatchers
   - Video on GPU, audio DSP on CPU, events on asyncio (all parallel)
   - Total latency = max(work_times), not sum(work_times)

This aligns with SMPTE ST 2110 and professional broadcast practice.

## Conclusion

Our current design captures many core actor model principles:
- Message passing ✅
- Encapsulation ✅
- At-most-once delivery ✅
- NO ordering guarantee ✅ (realtime-appropriate)
- Message dropping ✅ (realtime backpressure)

**Design Complete:**
- ✅ Concurrent execution model (4 dispatchers: Asyncio, ThreadPool, ProcessPool, GPU)
- ✅ GPU acceleration with CUDA streams and shader support
- ✅ Realtime semantics documented
- ✅ SMPTE 2110 network transport designed

**Implementation Needed (Phase 3):**
1. Lifecycle management (RUNNING, STOPPED, FAILED)
2. Parent-child relationships (spawn, auto-cleanup)
3. Optional supervision (failure recovery)
4. Dispatcher implementation (Asyncio, ThreadPool, ProcessPool, GPU)

**Future (Phase 4+):**
1. SMPTE 2110 RTP/UDP network actors
2. Actor addressing for network transparency

streamlib is now a fully-designed actor-based system suitable for distributed, concurrent, realtime stream processing with GPU acceleration.
