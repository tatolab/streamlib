# streamlib Project - Phase 3 Implementation

## Vision

**Composable realtime streaming SDK for Python with network-transparent operations.**

Unix-pipe-style primitives for realtime streams (video, audio, events, data) that AI agents can orchestrate.

```python
# Simple and composable
runtime = StreamRuntime(fps=30)
runtime.add_stream(Stream(camera_handler, dispatcher='asyncio'))
runtime.add_stream(Stream(blur_handler, dispatcher='threadpool'))
runtime.connect(camera_handler.outputs['video'], blur_handler.inputs['video'])
await runtime.start()
```

## Architecture

See `docs/architecture.md` for complete specification.

**Three-Layer Design:**
1. **StreamRuntime** - Lifecycle manager, capability negotiation, clock provider
2. **Stream** - Configuration wrapper (handler + dispatcher + transport)
3. **StreamHandler** - Pure processing logic (inert until runtime activates)

**Key Features:**
- Capability-based ports (`['cpu']`, `['gpu']`, `['cpu', 'gpu']`)
- Runtime capability negotiation (auto-inserts transfer handlers)
- Explicit dispatchers (`'asyncio'`, `'threadpool'`, `'gpu'`, `'processpool'`)
- Clock-driven processing (no message queues)
- Zero-copy ring buffers (references, not data copies)

## Implementation Plan

### Phase 3.1: Core Infrastructure ✅

**Goal**: Implement foundation for handler-based architecture

**Status**: COMPLETE (commit: bedfe89)

#### Tasks

1. **Ring Buffers** (`src/streamlib/buffers.py`) ✅
   - [x] `RingBuffer[T]` - Generic CPU ring buffer with type hints
   - [x] 3-slot circular buffer with latest-read semantics
   - [x] Thread-safe write/read operations
   - [x] `GPURingBuffer` - Zero-copy GPU memory (PyTorch)
   - [x] Pre-allocated GPU buffers with get_write_buffer/advance pattern
   - [x] Tests: write/read, overwrite behavior, thread safety (already existed)

2. **Message Types** (`src/streamlib/messages.py`) ✅
   - [x] `VideoFrame` - Video frame with metadata
   - [x] `AudioBuffer` - Audio samples with metadata
   - [x] `DataMessage` - Generic data messages
   - [x] Tests: validation, dataclass behavior (already existed)

3. **Clock Abstraction** (`src/streamlib/clocks.py`) ✅
   - [x] `Clock` - Abstract base class
   - [x] `TimedTick` - Dataclass with timestamp, frame_number, clock_source_id, fps
   - [x] `SoftwareClock` - Free-running software timer
   - [x] `PTPClock` (stub) - IEEE 1588 Precision Time Protocol
   - [x] `GenlockClock` (stub) - SDI hardware sync
   - [x] Tests: tick generation, timing accuracy, frame numbering (already existed)

4. **Dispatchers** (`src/streamlib/dispatchers.py`) ✅
   - [x] `Dispatcher` - Abstract base class
   - [x] `AsyncioDispatcher` - I/O-bound tasks (default)
   - [x] `ThreadPoolDispatcher` - CPU-bound tasks
   - [x] `ProcessPoolDispatcher` (stub) - Heavy compute
   - [x] `GPUDispatcher` (stub) - GPU-accelerated
   - [x] Tests: concurrent execution, shutdown behavior (already existed)

5. **Capability-Based Ports** (`src/streamlib/ports.py`) ✅
   - [x] `StreamOutput` - Output port with capabilities and negotiated_memory
   - [x] `StreamInput` - Input port with capabilities and negotiated_memory
   - [x] `VideoInput/VideoOutput` - Typed ports for 'video' port_type
   - [x] `AudioInput/AudioOutput` - Typed ports for 'audio' port_type
   - [x] `DataInput/DataOutput` - Typed ports for 'data' port_type
   - [x] Tests: port creation, capability declaration (pending)

6. **StreamHandler Base Class** (`src/streamlib/handler.py`) ✅
   - [x] `StreamHandler` - Abstract base class
   - [x] Inert until runtime activates
   - [x] `inputs`/`outputs` dictionaries for ports
   - [x] `async def process(tick: TimedTick)` - Abstract method
   - [x] Optional `on_start()`/`on_stop()` lifecycle hooks
   - [x] Internal `_activate()`, `_deactivate()`, `_run()` methods
   - [x] Tests: handler lifecycle, tick processing (pending)

7. **Stream Configuration** (`src/streamlib/stream.py`) ✅
   - [x] `Stream` - Configuration wrapper
   - [x] Wraps handler + dispatcher + transport (optional)
   - [x] Tests: stream creation, config storage (pending)

8. **Transfer Handlers** (`src/streamlib/transfers.py`) ✅
   - [x] `CPUtoGPUTransferHandler` - CPU → GPU memory transfer
   - [x] `GPUtoCPUTransferHandler` - GPU → CPU memory transfer
   - [x] Explicit port capabilities (`['cpu']` input, `['gpu']` output)
   - [x] Tests: memory transfer, type conversion (pending)

9. **StreamRuntime** (`src/streamlib/runtime.py`) ✅
   - [x] `StreamRuntime` - Lifecycle manager
   - [x] `add_stream(stream)` - Register and activate handlers
   - [x] `connect(output_port, input_port)` - Explicit wiring with capability negotiation
   - [x] Auto-insert transfer handlers when capabilities don't overlap
   - [x] Provide shared clock to all handlers
   - [x] Assign dispatchers based on Stream config
   - [x] `start()`, `stop()`, `run()` lifecycle methods
   - [x] Tests: lifecycle, capability negotiation, auto-transfer (pending)

**Dependencies**: None (uses only Python stdlib + numpy + torch)

**Time taken**: 1 session (documentation + implementation)

**Files created**:
- `packages/streamlib/src/streamlib/ports.py` (242 lines)
- `packages/streamlib/src/streamlib/handler.py` (228 lines)
- `packages/streamlib/src/streamlib/stream.py` (83 lines)
- `packages/streamlib/src/streamlib/transfers.py` (188 lines)
- `packages/streamlib/src/streamlib/runtime.py` (303 lines)

**Files updated**:
- `packages/streamlib/src/streamlib/__init__.py` - New exports for v0.2.0

**Note**: Comprehensive tests pending - prioritized implementation to prove architecture first

---

### Phase 3.2: Basic Handlers ⏳

**Goal**: Prove the architecture with working pipelines

#### Tasks

10. **TestPatternHandler** (`src/streamlib/handlers/test_pattern.py`)
    - [ ] Generate test patterns (SMPTE bars, gradient, black, white)
    - [ ] CPU-only port: `VideoOutput('video', capabilities=['cpu'])`
    - [ ] Tests: pattern generation, frame output

11. **DisplayHandler** (`src/streamlib/handlers/display.py`)
    - [ ] Display video in OpenCV window
    - [ ] CPU-only port: `VideoInput('video', capabilities=['cpu'])`
    - [ ] Apply macOS display fixes (cv2.startWindowThread, WINDOW_AUTOSIZE, etc.)
    - [ ] Tests: manual verification (window appears)

12. **Integration Demo** (`examples/demo_basic.py`)
    - [ ] TestPatternHandler → DisplayHandler pipeline
    - [ ] 640×480 @ 30 FPS (performance baseline)
    - [ ] Tests: pipeline runs, no dropped frames

**Dependencies**: Phase 3.1

**Estimated time**: 3-4 days

---

### Phase 3.3: Advanced Handlers ⏳

**Goal**: Implement complex processing handlers

#### Tasks

13. **BlurFilter** (`src/streamlib/handlers/blur.py`)
    - [ ] Gaussian blur using cv2 or numpy
    - [ ] Flexible ports: `capabilities=['cpu', 'gpu']`
    - [ ] Check `negotiated_memory` to adapt CPU/GPU processing
    - [ ] Tests: blur quality, CPU/GPU paths

14. **CompositorHandler** (`src/streamlib/handlers/compositor.py`)
    - [ ] N input ports (configurable, default 4)
    - [ ] Alpha blending with optimized numpy (from benchmark_results.md)
    - [ ] CPU-only initially: `capabilities=['cpu']`
    - [ ] Tests: alpha blending, layer ordering, performance

15. **DrawingHandler** (`src/streamlib/handlers/drawing.py`)
    - [ ] Python code execution with Skia
    - [ ] DrawingContext with time, frame_number, custom variables
    - [ ] CPU-only: `capabilities=['cpu']`
    - [ ] Tests: drawing execution, context variables

16. **Integration Demo** (`examples/demo_advanced.py`)
    - [ ] TestPattern → Blur → Compositor → Display
    - [ ] Multiple layers with drawing overlays
    - [ ] Tests: complex pipeline works, visual verification

**Dependencies**: Phase 3.2

**Estimated time**: 1 week

---

### Phase 3.4: GPU Support ⏳

**Goal**: Add GPU acceleration with capability negotiation

#### Tasks

17. **GPU Transfer Handlers** (already in 3.1)
    - [ ] Test CPU→GPU→CPU pipelines
    - [ ] Verify zero-copy behavior
    - [ ] Tests: memory transfer, no CPU copies

18. **GPU Blur** (`src/streamlib/handlers/blur_gpu.py`)
    - [ ] GPU-accelerated blur using PyTorch
    - [ ] GPU-only ports: `capabilities=['gpu']`
    - [ ] Tests: GPU processing, performance vs CPU

19. **Integration Demo** (`examples/demo_gpu.py`)
    - [ ] TestPattern (CPU) → Transfer → BlurGPU (GPU) → Transfer → Display (CPU)
    - [ ] Runtime auto-inserts transfer handlers
    - [ ] Tests: GPU pipeline works, capability negotiation

**Dependencies**: Phase 3.3, CUDA/PyTorch

**Estimated time**: 3-4 days

---

### Phase 3.5: File I/O ⏳

**Goal**: Read/write video files

#### Tasks

20. **FileReaderHandler** (`src/streamlib/handlers/file_reader.py`)
    - [ ] Read video files using PyAV
    - [ ] CPU output: `capabilities=['cpu']`
    - [ ] Tests: read various formats, frame extraction

21. **FileWriterHandler** (`src/streamlib/handlers/file_writer.py`)
    - [ ] Write video files using PyAV
    - [ ] CPU input: `capabilities=['cpu']`
    - [ ] Tests: write various formats, playback verification

22. **Integration Demo** (`examples/demo_files.py`)
    - [ ] FileReader → Blur → FileWriter pipeline
    - [ ] Tests: file processing, output verification

**Dependencies**: Phase 3.3, PyAV

**Estimated time**: 3-4 days

---

## Testing Strategy

### Unit Tests
- Each component tested in isolation
- Fast feedback (< 1s per test)
- High coverage (>90%)

### Integration Tests
- Multi-handler pipelines
- Capability negotiation scenarios
- Performance benchmarks

### Visual Tests
- Manual verification of video output
- Save frames to PNG for inspection
- Compare against expected output

### Performance Tests
- **Target**: 1080p30 < 33ms per frame (30 FPS)
- **Baseline**: 640×480 @ 30 FPS works reliably
- **Profile before optimizing**: Use cProfile to find bottlenecks

## Success Criteria

Phase 3 is complete when:

1. ✅ All core infrastructure implemented (3.1)
2. ✅ Basic handlers work (TestPattern → Display)
3. ✅ Capability negotiation works (auto-insert transfers)
4. ✅ Advanced handlers work (Compositor, Drawing)
5. ✅ GPU support works (CPU ↔ GPU transfers)
6. ✅ File I/O works (read/write video files)
7. ✅ All tests pass (unit + integration)
8. ✅ Performance meets baseline (640×480 @ 30 FPS)
9. ✅ Documentation complete
10. ✅ Demo applications work

## Timeline

**Total estimated time: 4-5 weeks**

- Week 1-2: Core infrastructure (3.1)
- Week 2: Basic handlers (3.2)
- Week 3: Advanced handlers (3.3)
- Week 4: GPU support (3.4)
- Week 4-5: File I/O (3.5)

## Future Phases

### Phase 4: Network Transparency
- NetworkSendHandler / NetworkReceiveHandler
- SMPTE ST 2110 RTP/UDP implementation
- Real PTP clock implementation
- Jitter buffers

### Phase 5: Production Features
- Audio handlers (capture, processing, output)
- HLS streaming
- WebRTC support
- Performance optimizations

## References

- **Architecture**: `docs/architecture.md` (authoritative spec)
- **Performance**: `benchmark_results.md` (optimization learnings)
- **Original vision**: `docs/markdown/conversation-history.md`
