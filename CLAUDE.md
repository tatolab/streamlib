# Context for Claude

## Project Vision

streamlib is a **composable streaming library for Python** based on the **Actor Pattern**. It provides Unix-pipe-style primitives for realtime streams (video, audio, events, data) that AI agents can orchestrate.

## Why This Exists

### The Problem

Most streaming/visual tools are **large, stateful, monolithic applications** (Unity, OBS, complex streaming platforms). They're environments, not primitives. There's no equivalent to "pipe grep into sed into awk" for visual operations.

### The Solution

**Independent actors that communicate via messages** - like Unix tools but for realtime streams:

```bash
# Unix philosophy (text):
cat file.txt | grep "error" | sed 's/ERROR/WARNING/' | awk '{print $1}'

# streamlib (realtime streams):
VideoGeneratorActor() >> CompositorActor() >> DisplayActor()
AudioGeneratorActor() >> MixerActor() >> SpeakerActor()
DisplayActor().outputs['keyboard'] >> KeyHandlerActor()
```

### Core Philosophy

1. **Actor-based**: Each component is an independent actor processing messages
2. **Message-passing only**: No shared state, communication via mailboxes
3. **Composable primitives**: Small, single-purpose actors that connect together
4. **Network-transparent**: Operations work seamlessly locally or remotely
5. **Distributed**: Chain operations across machines (phone → edge → cloud)
6. **Zero-dependency core**: No GStreamer required (uses PyAV for codecs)
7. **Tool-first**: Provide tools, let use cases emerge
8. **AI-orchestratable**: Agents can dynamically create and connect actors

## Key Design Insights

### Actor Pattern

> "The stream (clock) always flows, whether your boat (actor) participates or not."

**Actors are like boats in a river:**
- Stream = Always flowing (clock ticks continuously)
- Boat = Actor (drops in, floats along, processes messages)
- Bathtub = Isolated actor (creates own clock when not connected)
- Upstream/Downstream = Message flow (actors affect downstream, unaware of upstream)

### Emergent Capabilities

> "You don't need to invent AI-specific tools. The tools already exist. You just need to make them accessible to something that can reason about orchestrating them."

We're not building "yet another streaming platform." We're building **composable primitives that AI can orchestrate**.

> "Anthropic didn't build Claude Code thinking 'users will debug React.' They gave tools (read, write, bash, grep) and use cases emerged."

Same approach here: give tools for stream processing, let use cases emerge.

### Unix Philosophy Applied

**Similar to Unix Tools**: `grep` doesn't maintain state. It takes input, produces output, done.

**streamlib actors work the same way:**
- Independent, continuously running
- Read from inputs (mailboxes)
- Process internally
- Write to outputs (send messages)
- Don't know or care what's upstream/downstream

## Architecture

**Actor Pattern** - See `docs/architecture.md` for complete details.

Key principles:
1. **Actors run continuously** - Created and immediately processing (no start/stop)
2. **Message-based communication** - No shared state, only async messages
3. **Mailbox processing** - Each input is a queue, messages processed sequentially
4. **Encapsulated state** - Private to each actor
5. **Concurrent execution** - All actors run independently

### Core Abstractions

```python
# Actor - base class for all components
class Actor(ABC):
    inputs: Dict[str, StreamInput]   # Mailboxes (receive messages)
    outputs: Dict[str, StreamOutput] # Send messages
    clock: Clock                     # Time synchronization

    async def process(self, tick: TimedTick):
        """Override: process one tick"""
        pass

# Message types
VideoFrame, AudioBuffer, KeyEvent, MouseEvent, DataMessage

# Connection (pipe operator)
upstream.outputs['video'] >> downstream.inputs['video']
```

### Automatic Behavior

**Actors auto-start when created:**
```python
# Wrong - old pattern
actor = SomeActor()
await actor.start()  # ❌ No start()
await actor.run()    # ❌ No run()
await actor.stop()   # ❌ No stop()

# Right - actor pattern
actor = SomeActor()  # ✅ Created and immediately processing
```

**Clock syncs automatically:**
```python
# Isolated actor (bathtub mode)
actor = VideoGeneratorActor()  # Creates own SoftwareClock

# Connected actors
gen >> display  # display syncs to gen's clock automatically
```

### Concurrent Execution & Dispatchers

**Actors run on different execution models based on workload:**

```python
# I/O-bound (default): Network, file, events
rtp_recv = RTPReceiveActor()  # AsyncioDispatcher (default)

# CPU-bound: Encoding, audio DSP
encoder = H264EncoderActor(
    dispatcher=ThreadPoolDispatcher(workers=4)
)

# GPU-accelerated: ML inference, shaders, GPU encoding
face_detect = FaceDetectorActor(
    dispatcher=GPUDispatcher(device='cuda:0', streams=4)
)
shader = ShaderEffectActor(
    'bloom.glsl',
    dispatcher=GPUDispatcher(device='cuda:0')
)

# Chain: I/O → CPU → GPU → GPU → I/O
rtp_recv >> encoder >> face_detect >> shader >> display
```

**Four dispatcher types:**
1. **AsyncioDispatcher** (I/O-bound) - Network, file, events - Default
2. **ThreadPoolDispatcher** (CPU-bound) - Video encoding, audio DSP
3. **ProcessPoolDispatcher** (Heavy CPU) - Multi-pass encoding
4. **GPUDispatcher** (GPU) - ML inference, shaders, GPU encoding

**GPU optimization:**
- Keep data on GPU across actors (minimize CPU↔GPU transfers)
- CUDA streams for concurrent GPU execution
- Shader support (OpenGL/Vulkan/Metal) for realtime effects

See `docs/architecture.md` "Concurrent Execution & Dispatchers" section for full details.

## Current Status

### Phase 1-2: COMPLETE ✅
- Base infrastructure (clocks, timing, layers, compositor)
- Sources & sinks (test patterns, file I/O, display, HLS)
- 17/17 tests passing

### Phase 3: Actor Refactor 🚧 IN PROGRESS
- Converting pipeline pattern to Actor pattern
- See `docs/project.md` TODO section for details

### Phase 4: Network-Transparent 📋 PLANNED
- NetworkSendActor / NetworkReceiveActor
- Distributed stream processing

## Important Files

### Documentation
- **`docs/architecture.md`** - Complete Actor-based architecture
- **`docs/project.md`** - Project overview, status, TODOs
- **`docs/markdown/conversation-history.md`** - Original vision (session 23245f82)

### Code
- `src/streamlib/` - Main library package
- `demo.py` - Visual demos
- `tests/` - Test suite
- `pyproject.toml` - Dependencies

## Usage Example (Target API)

```python
# Actors auto-start when created
video_gen = VideoGeneratorActor(pattern='smpte_bars', fps=60)
compositor = CompositorActor(width=1920, height=1080)
display = DisplayActor(window_name='Output')

# Add layers to compositor
compositor.add_layer(CircleLayer())
compositor.add_layer(TextLayer("streamlib"))

# Connect (pipe operator)
video_gen.outputs['video'] >> compositor.inputs['base']
compositor.outputs['video'] >> display.inputs['video']

# Handle events
display.outputs['keyboard'] >> key_logger.inputs['events']

# Already running! Just wait
await asyncio.Event().wait()
```

## Multi-Stream Example

```python
# Video path
video_gen >> compositor >> display

# Audio path
audio_gen >> mixer >> speaker

# Event paths
display.outputs['keyboard'] >> key_handler
display.outputs['mouse'] >> mouse_tracker

# All running concurrently, synchronized by clock
```

## Development Principles

1. **Build then optimize** - Don't imagine performance problems
2. **Actor pattern** - Everything is an independent actor
3. **Message passing** - No shared state, only messages
4. **Network-transparent** - Design for distributed from day one
5. **Visual verification** - Save frames, verify output
6. **Test incrementally** - Test each actor in isolation
7. **Let use cases emerge** - Don't predict, discover

## When Working on This Project

1. **Remember**: Actors, not pipelines
2. **Think**: Independent actors running continuously
3. **Design**: Message-passing only (no shared state)
4. **Ensure**: Actors auto-start in `__init__`
5. **Verify**: Works locally AND over network
6. **Ask**: Can an AI agent create and connect this dynamically?

## Philosophy Checks

Before implementing any feature:
- ✅ Is it an independent actor?
- ✅ Does it run continuously from creation?
- ✅ Does it communicate only via messages?
- ✅ Is state fully encapsulated?
- ✅ Can agents create and connect it dynamically?
- ✅ Does it work locally and over network?
- ✅ Is it like `grep | sed | awk` for streams?
- ✅ Does it use the right dispatcher (I/O, CPU, GPU)?
- ✅ For GPU actors, does it minimize CPU↔GPU transfers?

If any answer is "no", the design is wrong.

## References

- **Project docs**: `docs/project.md` (complete overview + TODOs)
- **Architecture**: `docs/architecture.md` (Actor pattern details)
- **Original vision**: `docs/markdown/conversation-history.md`
- Actor Model: https://en.wikipedia.org/wiki/Actor_model
- Akka: https://doc.akka.io/
- WebRTC: https://webrtc.org/
- Unix Philosophy: https://en.wikipedia.org/wiki/Unix_philosophy
