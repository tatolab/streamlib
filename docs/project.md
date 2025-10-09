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

### Phase 3.1: Core Infrastructure ⏳

**Goal**: Implement foundation for handler-based architecture

#### Tasks

1. **Ring Buffers** (`src/streamlib/buffers.py`)
   - [ ] `RingBuffer[T]` - Generic CPU ring buffer with type hints
   - [ ] 3-slot circular buffer with latest-read semantics
   - [ ] Thread-safe write/read operations
   - [ ] `GPURingBuffer` - Zero-copy GPU memory (PyTorch)
   - [ ] Pre-allocated GPU buffers with get_write_buffer/advance pattern
   - [ ] Tests: write/read, overwrite behavior, thread safety

2. **Message Types** (`src/streamlib/messages.py`)
   - [ ] `VideoFrame` - Video frame with metadata
   - [ ] `AudioBuffer` - Audio samples with metadata
   - [ ] `DataMessage` - Generic data messages
   - [ ] Tests: validation, dataclass behavior

3. **Clock Abstraction** (`src/streamlib/clocks.py`)
   - [ ] `Clock` - Abstract base class
   - [ ] `TimedTick` - Dataclass with timestamp, frame_number, clock_source_id, fps
   - [ ] `SoftwareClock` - Free-running software timer
   - [ ] `PTPClock` (stub) - IEEE 1588 Precision Time Protocol
   - [ ] `GenlockClock` (stub) - SDI hardware sync
   - [ ] Tests: tick generation, timing accuracy, frame numbering

4. **Dispatchers** (`src/streamlib/dispatchers.py`)
   - [ ] `Dispatcher` - Abstract base class
   - [ ] `AsyncioDispatcher` - I/O-bound tasks (default)
   - [ ] `ThreadPoolDispatcher` - CPU-bound tasks
   - [ ] `ProcessPoolDispatcher` (stub) - Heavy compute
   - [ ] `GPUDispatcher` (stub) - GPU-accelerated
   - [ ] Tests: concurrent execution, shutdown behavior

5. **Capability-Based Ports** (`src/streamlib/ports.py`)
   - [ ] `StreamOutput` - Output port with capabilities and negotiated_memory
   - [ ] `StreamInput` - Input port with capabilities and negotiated_memory
   - [ ] `VideoInput/VideoOutput` - Typed ports for 'video' port_type
   - [ ] `AudioInput/AudioOutput` - Typed ports for 'audio' port_type
   - [ ] `DataInput/DataOutput` - Typed ports for 'data' port_type
   - [ ] Tests: port creation, capability declaration

6. **StreamHandler Base Class** (`src/streamlib/handler.py`)
   - [ ] `StreamHandler` - Abstract base class
   - [ ] Inert until runtime activates
   - [ ] `inputs`/`outputs` dictionaries for ports
   - [ ] `async def process(tick: TimedTick)` - Abstract method
   - [ ] Optional `on_start()`/`on_stop()` lifecycle hooks
   - [ ] Internal `_activate()`, `_deactivate()`, `_run()` methods
   - [ ] Tests: handler lifecycle, tick processing

7. **Stream Configuration** (`src/streamlib/stream.py`)
   - [ ] `Stream` - Configuration wrapper
   - [ ] Wraps handler + dispatcher + transport (optional)
   - [ ] Tests: stream creation, config storage

8. **Transfer Handlers** (`src/streamlib/transfers.py`)
   - [ ] `CPUtoGPUTransferHandler` - CPU → GPU memory transfer
   - [ ] `GPUtoCPUTransferHandler` - GPU → CPU memory transfer
   - [ ] Explicit port capabilities (`['cpu']` input, `['gpu']` output)
   - [ ] Tests: memory transfer, type conversion

9. **StreamRuntime** (`src/streamlib/runtime.py`)
   - [ ] `StreamRuntime` - Lifecycle manager
   - [ ] `add_stream(stream)` - Register and activate handlers
   - [ ] `connect(output_port, input_port)` - Explicit wiring with capability negotiation
   - [ ] Auto-insert transfer handlers when capabilities don't overlap
   - [ ] Provide shared clock to all handlers
   - [ ] Assign dispatchers based on Stream config
   - [ ] `start()`, `stop()`, `run()` lifecycle methods
   - [ ] Tests: lifecycle, capability negotiation, auto-transfer

**Dependencies**: None (uses only Python stdlib + numpy + torch)

**Estimated time**: 1-2 weeks

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
