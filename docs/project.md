# streamlib Project

## Vision

**Composable streaming library for Python with network-transparent operations.**

Unix-pipe-style primitives for realtime streams (video, audio, events, data) that AI agents can orchestrate.

```python
# Like Unix pipes but for realtime streams
video_gen >> compositor >> display
audio_gen >> mixer >> speaker
keyboard >> event_handler
```

## Why This Exists

### The Problem
Most streaming/visual tools are large, stateful, monolithic applications (Unity, OBS, streaming platforms). They're environments, not primitives. There's no equivalent to `grep | sed | awk` for visual/audio operations.

### The Solution
**Stateless primitives that can be orchestrated** - like Unix tools but for streams:

```bash
# Unix philosophy (text)
cat file.txt | grep "error" | sed 's/ERROR/WARNING/' | awk '{print $1}'

# streamlib (realtime streams)
VideoGenerator() >> Compositor(layers) >> Display()
```

### Core Philosophy
1. **Actor-based**: Each component is an independent actor processing messages
2. **Composable primitives**: Small, single-purpose components that connect together
3. **Network-transparent**: Operations work seamlessly locally or remotely
4. **Distributed**: Chain operations across machines (phone → edge → cloud)
5. **Zero-dependency core**: No GStreamer required (uses PyAV for codecs)
6. **Tool-first**: Provide tools, let use cases emerge
7. **AI-orchestratable**: Agents can dynamically create and connect actors

## Architecture

**Actor Pattern** - See `docs/architecture.md` for complete details.

Key principles:
- Actors run continuously from creation (no start/stop)
- Message-based communication only (no shared state)
- Mailbox processing (sequential message handling)
- Encapsulated state (private to each actor)
- Concurrent and independent execution

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

# Connection
upstream.outputs['video'] >> downstream.inputs['video']
```

## Current Status

### Phase 1: Core Infrastructure ✅ COMPLETE
- [x] Base abstractions (TimestampedFrame, Layer, Compositor)
- [x] Drawing layers with Skia
- [x] Alpha blending compositor
- [x] Plugin system
- [x] Clock and timing infrastructure
- [x] Tests (9/9 passing)

### Phase 2: Basic Sources & Sinks ✅ COMPLETE
- [x] TestSource (SMPTE bars, gradients, moving patterns)
- [x] FileSource (read video files with PyAV)
- [x] FileSink (write video files)
- [x] DisplaySink (OpenCV window for preview)
- [x] HLSSink (HTTP Live Streaming)
- [x] Stream orchestrator (drives compositor with clock)
- [x] Tests (17/17 passing)

### Phase 3: Actor Refactor 🚧 IN PROGRESS

**Goal**: Refactor existing code to Actor pattern

**Current Issues**:
1. ❌ Components use Source/Sink pattern (not Actor)
2. ❌ Manual lifecycle (`await stream.run()`)
3. ❌ Centralized orchestrator (`Stream` class)
4. ❌ Video-centric (no audio, events, data)
5. ❌ DisplaySink blocks event loop (`cv2.waitKey()`)

**Target**:
1. ✅ Actor base class with auto-start
2. ✅ Named inputs/outputs with mailboxes
3. ✅ Message types (VideoFrame, AudioBuffer, Event, Data)
4. ✅ Automatic clock synchronization
5. ✅ Pipe operator (`>>`) for connections
6. ✅ Multi-stream support (video + audio + events + data)

### Phase 4: Network-Transparent 📋 PLANNED
- [ ] NetworkSendActor (send streams to remote)
- [ ] NetworkReceiveActor (receive streams from remote)
- [ ] Serializable message format
- [ ] Compression (JPEG, H.264, Opus)
- [ ] Clock sync across network (PTP metadata)
- [ ] Discovery/registration (optional)

### Phase 5: Advanced Actors 📋 PLANNED
- [ ] WebcamActor
- [ ] ScreenCaptureActor
- [ ] AudioMixerActor
- [ ] VideoFilterActors (blur, sharpen, color correct)
- [ ] MLActors (face detection, object tracking, depth estimation)
- [ ] SupervisorActor (monitor and restart failed actors)

## TODO List

### Immediate (Phase 3)

#### Architecture
- [ ] Create `Actor` base class
  - Auto-start in `__init__` (no manual `run()`)
  - `_acquire_clock()` for automatic sync
  - `process(tick)` abstract method
  - Internal `_run()` loop

- [ ] Create `StreamInput` (Mailbox)
  - `async read()` - blocking read
  - `async try_read()` - non-blocking read
  - `has_messages()` - check if empty
  - `queue` - asyncio.Queue

- [ ] Create `StreamOutput` (Message sender)
  - `async send(message)` - send to subscribers
  - `connect(input)` - connect to mailbox
  - `__rshift__(input)` - pipe operator support

- [ ] Define message types
  - `VideoFrame` dataclass
  - `AudioBuffer` dataclass
  - `KeyEvent` dataclass
  - `MouseEvent` dataclass
  - `DataMessage` dataclass

#### Actor Implementations

- [ ] Refactor `TestSource` → `VideoGeneratorActor`
  - No inputs
  - Output: `video` (VideoFrame)
  - Generates test patterns continuously

- [ ] Refactor `DisplaySink` → `DisplayActor`
  - Input: `video` (VideoFrame)
  - Outputs: `keyboard` (KeyEvent), `mouse` (MouseEvent)
  - Fix `cv2.waitKey()` blocking (already done - keep `await asyncio.sleep(0)`)
  - Emit keyboard/mouse events to outputs

- [ ] Refactor `DefaultCompositor` → `CompositorActor`
  - Input: `base` (VideoFrame, optional)
  - Dynamic inputs: `layer_N` (VideoFrame)
  - Output: `video` (VideoFrame)
  - Maintain layers as private state

- [ ] Refactor `DrawingLayer` → `DrawingActor`
  - No inputs
  - Output: `video` (VideoFrame)
  - Executes Python drawing code each tick

- [ ] Refactor `FileSource` → `FileReaderActor`
  - No inputs
  - Output: `video` (VideoFrame)
  - Loops or stops at end (configurable)

- [ ] Refactor `FileSink` → `FileWriterActor`
  - Input: `video` (VideoFrame)
  - No outputs
  - Writes to file using PyAV

- [ ] Refactor `HLSSink` → `HLSActor`
  - Input: `video` (VideoFrame)
  - Output: `segments` (DataMessage, optional)
  - Generates HLS segments

#### Audio Support

- [ ] Create `AudioGeneratorActor`
  - No inputs
  - Output: `audio` (AudioBuffer)
  - Generate tones, noise, silence

- [ ] Create `AudioMixerActor`
  - Inputs: `audio_N` (AudioBuffer, multiple)
  - Output: `audio` (AudioBuffer)
  - Mix multiple audio streams

- [ ] Create `AudioOutputActor`
  - Input: `audio` (AudioBuffer)
  - No outputs
  - Play audio through speakers (pyaudio or sounddevice)

#### Clock Improvements

- [ ] Implement `UpstreamClock`
  - Syncs to clock metadata in messages
  - Used when actor has upstream connections

- [ ] Add clock metadata to all messages
  - `clock_source_id` field
  - `clock_rate` field

#### Testing

- [ ] Update existing tests for Actor pattern
- [ ] Test automatic clock sync
- [ ] Test mailbox processing
- [ ] Test pipe operator (`>>`)
- [ ] Test multi-stream (video + audio + events)
- [ ] Test isolated actors (bathtub mode)
- [ ] Test network transparency (local first)

#### Documentation

- [ ] Update examples in CLAUDE.md
- [ ] Create migration guide (old → new)
- [ ] Add Actor pattern examples
- [ ] Document clock synchronization
- [ ] Document message types

#### Demo

- [ ] Rewrite `demo.py` using Actors
  - No `await stream.run()`
  - Actors auto-start
  - Show event handling
  - Show audio + video together

### Later (Phase 4+)

- [ ] Network actors (send/receive)
- [ ] Compression actors (H.264, JPEG, Opus)
- [ ] Discovery/registration service
- [ ] Supervisor pattern (failure recovery)
- [ ] ML actors (face detection, etc)
- [ ] WebRTC integration
- [ ] Browser-based display (WebSocket + Canvas)
- [ ] Performance benchmarks
- [ ] More complete test coverage
- [ ] CI/CD pipeline

### Known Issues

1. **DisplaySink blocks event loop** ✅ FIXED
   - `cv2.waitKey()` is blocking call
   - Fixed with `await asyncio.sleep(0)` before waitKey
   - Allows timeout tasks to run

2. **No automatic transitions in demo** ✅ FIXED
   - `run_for_duration()` timeout wasn't firing
   - Root cause: DisplaySink blocking event loop
   - Fixed with async yield

3. **Performance (26 FPS compositor)** ⚠️ ACCEPTABLE
   - Bottleneck: numpy alpha blending (not drawing)
   - Optimized with uint16 arithmetic: 11.8 FPS → 26.9 FPS (2.3x)
   - Good enough for dev/debug
   - Can optimize further later if needed

4. **OpenCV not async-friendly** ⚠️ WORKAROUND
   - `cv2.waitKey()` blocks event loop
   - Workaround: `await asyncio.sleep(0)` before call
   - Better solution: Replace with pyglet/SDL2 (future)
   - For now, OpenCV is fine for dev/debug

## Reusable Code

### Keep (Already Actor-Compatible)

- ✅ **Clock infrastructure** (`timing.py`)
  - `Clock`, `SoftwareClock`, `TimedTick`
  - Already async-friendly
  - Just needs `UpstreamClock` addition

- ✅ **Drawing with Skia** (`drawing.py`)
  - `DrawingContext`, `DrawingLayer`
  - Can be internal state of `DrawingActor`

- ✅ **Compositor logic** (`compositor.py`)
  - `_alpha_blend()`, `_composite()`
  - Can be private methods of `CompositorActor`
  - Optimizations already done

- ✅ **PyAV integration** (`sources/file_source.py`, `sinks/file_sink.py`)
  - Video file reading/writing
  - Can be internals of `FileReaderActor`/`FileWriterActor`

- ✅ **HLS generation** (`sinks/hls_sink.py`)
  - Segment creation logic
  - Can be internal to `HLSActor`

- ✅ **Plugin system** (`plugins.py`)
  - `@register_source`, `@register_sink`
  - Adapt to `@register_actor`

### Refactor (Convert to Actor)

- 🔄 **Base classes** (`base.py`)
  - Remove: `StreamSource`, `StreamSink` abstractions
  - Add: `Actor` base class
  - Add: `StreamInput`, `StreamOutput`, message types

- 🔄 **Stream orchestrator** (`stream.py`)
  - Remove: `Stream` class (centralized orchestrator)
  - Add: Connection helper functions (optional)
  - Add: Pipeline builder (optional, for convenience)

- 🔄 **All sources** → Actors with outputs
  - `TestSource` → `VideoGeneratorActor`
  - `FileSource` → `FileReaderActor`

- 🔄 **All sinks** → Actors with inputs
  - `DisplaySink` → `DisplayActor` (+ event outputs)
  - `FileSink` → `FileWriterActor`
  - `HLSSink` → `HLSActor`

- 🔄 **Compositor** → Actor
  - `DefaultCompositor` → `CompositorActor`
  - Keep compositing logic, wrap in Actor

### Remove (Wrong Pattern)

- ❌ **Stream class** (`stream.py`)
  - Central orchestrator anti-pattern
  - Actors run themselves, no orchestrator needed

- ❌ **Manual lifecycle**
  - No `await stream.run()`
  - No `await sink.start()` / `await sink.stop()`
  - Actors auto-start in `__init__`

## Migration Strategy

### Step 1: Add Actor Alongside Existing ✅
- Create `actor.py` with `Actor`, `StreamInput`, `StreamOutput`
- Don't break existing code
- Both patterns coexist

### Step 2: Create Example Actors ✅
- Implement 2-3 actors (VideoGenerator, Display, Compositor)
- Show side-by-side comparison with old pattern
- Update one demo to use actors

### Step 3: Full Migration 🚧
- Convert all sources/sinks to actors
- Update all demos
- Update tests
- Deprecate old pattern

### Step 4: Remove Old Code
- Delete `Stream` class
- Delete `StreamSource`/`StreamSink` abstractions
- Clean up

## Development Principles

1. **Build then optimize** - Don't imagine performance problems
2. **Start simple** - Basic primitives first, add complexity as needed
3. **Visual verification** - Save frames to files, verify output
4. **Test incrementally** - Test each component in isolation
5. **Actor pattern** - Everything is an independent actor
6. **Message passing** - No shared state, only messages
7. **Network-transparent** - Design for distributed from day one

## Design Philosophy

> "You don't need to invent AI-specific tools. The tools already exist. You just need to make them accessible to something that can reason about orchestrating them."

We're not building "yet another streaming platform." We're building **composable primitives that AI can orchestrate**.

Key insight: **Emergent behaviors from simple tools**
- Anthropic didn't build Claude Code thinking "users will debug React"
- They gave tools (read, write, bash, grep)
- Use cases emerged from actual usage

Same approach here:
- Give tools for stream processing (actors)
- Let use cases emerge
- AI agents can create and connect actors dynamically

## Examples

### Basic Video Pipeline
```python
# Actors auto-start when created
gen = VideoGeneratorActor(pattern='smpte_bars', fps=60)
display = DisplayActor(window_name='Output')

# Connect
gen.outputs['video'] >> display.inputs['video']

# Already running, just wait
await asyncio.Event().wait()  # Run forever
```

### With Compositor
```python
gen = VideoGeneratorActor(pattern='gradient')
comp = CompositorActor(width=1920, height=1080)
disp = DisplayActor()

# Add drawing layers
comp.add_layer(CircleLayer())
comp.add_layer(TextLayer("Hello"))

# Connect
gen >> comp >> disp
```

### Multi-Stream (Video + Audio + Events)
```python
# Video path
video_gen >> compositor >> display

# Audio path
audio_gen >> mixer >> speaker

# Event path
display.outputs['keyboard'] >> key_logger
display.outputs['mouse'] >> mouse_tracker
```

### Network-Transparent
```python
# Local machine
video_gen >> NetworkSendActor(remote='192.168.1.100:5000')

# Remote machine
NetworkReceiveActor(port=5000) >> display

# Clock and frames flow across network
```

### Agent-Orchestrated
```python
# Agent generates custom actor on the fly
code = await agent.generate("""
Create an actor that:
- Reads video
- Detects faces
- Draws bounding boxes
- Outputs annotated video
""")

FaceDetectorActor = eval(code)
detector = FaceDetectorActor()

# Connect
webcam >> detector >> display
```

## References

- Original vision: `docs/markdown/conversation-history.md` (session 23245f82)
- Architecture: `docs/architecture.md`
- Actor Model: https://en.wikipedia.org/wiki/Actor_model
- Akka: https://doc.akka.io/
- WebRTC: https://webrtc.org/
- Unix Philosophy: https://en.wikipedia.org/wiki/Unix_philosophy
- FastRTC (inspiration): Network-transparent WebRTC components

## Repository Structure

```
gst-mcp-tools/
├── src/streamlib/
│   ├── __init__.py           # Public API
│   ├── actor.py              # Actor base class [NEW]
│   ├── messages.py           # Message types [NEW]
│   ├── base.py               # Old abstractions [DEPRECATE]
│   ├── timing.py             # Clock infrastructure
│   ├── drawing.py            # Skia drawing
│   ├── compositor.py         # Compositing logic
│   ├── plugins.py            # Plugin system
│   ├── sources/              # Old sources [MIGRATE]
│   ├── sinks/                # Old sinks [MIGRATE]
│   └── actors/               # New actors [NEW]
│       ├── video.py          # Video actors
│       ├── audio.py          # Audio actors
│       ├── io.py             # File I/O actors
│       ├── network.py        # Network actors
│       └── ml.py             # ML actors
├── tests/
│   ├── test_streamlib_core.py      # Core tests
│   ├── test_actors.py              # Actor tests [NEW]
│   └── test_phase2_sources_sinks.py # Old tests
├── docs/
│   ├── architecture.md       # Complete architecture [NEW]
│   ├── project.md            # This file [NEW]
│   └── markdown/             # Legacy docs
│       └── conversation-history.md  # Original vision
├── demo.py                   # Main demo
└── pyproject.toml            # Dependencies
```

## Dependencies

### Core
- `numpy` - Array operations
- `skia-python` - 2D drawing
- `av` (PyAV) - Video codecs (FFmpeg)
- `opencv-python` - Display (temporary, may replace)

### Optional
- `pytest` - Testing
- `pytest-asyncio` - Async testing

### Future
- `pyaudio` or `sounddevice` - Audio output
- `pyglet` or `pysdl2` - Better display (replace OpenCV)
- `websockets` - Network transport
- `protobuf` or `msgpack` - Message serialization

## Getting Started

```bash
# Install
poetry install

# Run demo
poetry run python demo.py

# Run tests
poetry run pytest

# Format
poetry run black src/ tests/

# Type check
poetry run mypy src/
```

## Contributing

This is an experimental project exploring:
1. Composable streaming primitives
2. Actor-based architecture for realtime streams
3. AI agent orchestration of stream processing
4. Network-transparent distributed streams

Contributions welcome, especially:
- Actor implementations (video, audio, ML, etc)
- Network transport actors
- Performance optimizations
- Agent integration examples
- Documentation improvements

## License

MIT (see LICENSE file)

## Contact

Project: https://github.com/tatolab/gst-mcp-tools
