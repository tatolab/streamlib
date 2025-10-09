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

### Phase 3.2: Basic Handlers ✅

**Goal**: Prove the architecture with working pipelines

**Status**: COMPLETE (commit: 4c51d1a)

#### Tasks

10. **TestPatternHandler** (`src/streamlib/handlers/test_pattern.py`) ✅
    - [x] Generate test patterns (SMPTE bars, gradient, solid, checkerboard)
    - [x] CPU-only port: `VideoOutput('video', capabilities=['cpu'])`
    - [x] 640×480 default resolution
    - [x] Pre-generates static patterns for performance
    - [x] Tests: pattern generation verified (150 frames @ 30 FPS)

11. **DisplayHandler** (`src/streamlib/handlers/display.py`) ✅
    - [x] Display video in OpenCV window
    - [x] CPU-only port: `VideoInput('video', capabilities=['cpu'])`
    - [x] Apply macOS display fixes (cv2.startWindowThread fallback, WINDOW_AUTOSIZE, WND_PROP_TOPMOST)
    - [x] RGB→BGR conversion for OpenCV
    - [x] Tests: lifecycle verified (pending visual verification with proper opencv)

12. **Integration Demo** (`examples/demo_basic.py`) ✅
    - [x] TestPatternHandler → DisplayHandler pipeline
    - [x] 640×480 @ 30 FPS (performance baseline)
    - [x] Tests: pipeline runs successfully, 150 frames generated in 5 seconds

**Dependencies**: Phase 3.1

**Time taken**: 1 session

**Files created**:
- `packages/streamlib/src/streamlib/handlers/__init__.py` (13 lines)
- `packages/streamlib/src/streamlib/handlers/test_pattern.py` (195 lines)
- `packages/streamlib/src/streamlib/handlers/display.py` (120 lines)
- `examples/demo_basic.py` (64 lines)

**Files updated**:
- `packages/streamlib/src/streamlib/__init__.py` - Added Phase 3.2 handler exports

**Validation**:
- ✅ Pipeline successfully processes 150 frames (5s @ 30 FPS)
- ✅ Runtime lifecycle working correctly
- ✅ Capability negotiation working (CPU → CPU connection)
- ✅ Handler activation/deactivation working
- ⚠️  Visual display verification pending (opencv environment issue)

---

### Phase 3.3: Advanced Handlers ✅

**Goal**: Implement complex processing handlers

**Status**: COMPLETE (commit: 4a5a6fd)

#### Tasks

13. **BlurFilter** (`src/streamlib/handlers/blur.py`) ✅
    - [x] Gaussian blur using cv2 (CPU) or torch (GPU)
    - [x] Flexible ports: `capabilities=['cpu', 'gpu']` - adapts based on available libraries
    - [x] Check `negotiated_memory` to select CPU or GPU processing path
    - [x] CPU path: cv2.GaussianBlur (fast)
    - [x] GPU path: torch conv2d with Gaussian kernel
    - [x] Tests: Pipeline structure validated

14. **CompositorHandler** (`src/streamlib/handlers/compositor.py`) ✅
    - [x] N input ports (configurable, default 4)
    - [x] Alpha blending with optimized float32 numpy operations
    - [x] Pre-allocated accumulator buffer for performance
    - [x] CPU-only: `capabilities=['cpu']`
    - [x] Tests: Pipeline structure validated

15. **DrawingHandler** (`src/streamlib/handlers/drawing.py`) ✅
    - [x] Python code execution with cv2 drawing backend
    - [x] DrawingContext with time, frame_number, width, height, variables
    - [x] Drawing primitives: rectangle, circle, line, text
    - [x] CPU-only: `capabilities=['cpu']`
    - [x] Tests: Pipeline structure validated

16. **Integration Demo** (`examples/demo_advanced.py`) ✅
    - [x] Complex pipeline: TestPattern → Blur → Compositor ← Drawing → Display
    - [x] Multiple layers with animated procedural overlay
    - [x] Custom drawing function with rotating square and pulsing circle
    - [x] Tests: Pipeline structure validated, handlers load and connect

**Dependencies**: Phase 3.2

**Time taken**: 1 session

**Files created**:
- `packages/streamlib/src/streamlib/handlers/blur.py` (215 lines)
- `packages/streamlib/src/streamlib/handlers/compositor.py` (169 lines)
- `packages/streamlib/src/streamlib/handlers/drawing.py` (215 lines)
- `examples/demo_advanced.py` (139 lines)

**Files updated**:
- `packages/streamlib/src/streamlib/handlers/__init__.py` - Added Phase 3.3 exports
- `packages/streamlib/src/streamlib/__init__.py` - Added Phase 3.3 exports

**Validation**:
- ✅ Handlers implement flexible capability system
- ✅ BlurFilter adapts to CPU/GPU based on negotiated_memory
- ✅ CompositorHandler supports N-layer composition
- ✅ DrawingHandler executes Python code with drawing context
- ✅ Complex pipeline structure validated
- ⚠️  Visual verification pending proper opencv environment

---

### Phase 3.4: GPU Support ✅

**Goal**: Add GPU acceleration with capability negotiation

**Status**: COMPLETE (commit: pending)

#### Tasks

17. **GPU Transfer Handlers** (from Phase 3.1) ✅
    - [x] CPUtoGPUTransferHandler - numpy → torch.Tensor on GPU
    - [x] GPUtoCPUTransferHandler - torch.Tensor on GPU → numpy
    - [x] Runtime auto-insertion when no capability overlap
    - [x] Tests: Architecture validated, transfer logic implemented

18. **GPU Blur** (`src/streamlib/handlers/blur_gpu.py`) ✅
    - [x] GPU-accelerated blur using PyTorch conv2d
    - [x] GPU-only ports: `capabilities=['gpu']` (forces transfer insertion)
    - [x] Gaussian kernel created on GPU with torch operations
    - [x] Conditional import (gracefully handles missing PyTorch)
    - [x] Tests: Architecture validated (CUDA hardware testing pending)

19. **Integration Demo** (`examples/demo_gpu.py`) ✅
    - [x] Pipeline: TestPattern (CPU) → [CPUtoGPU] → BlurGPU (GPU) → [GPUtoCP] → Display (CPU)
    - [x] Runtime auto-inserts both transfer handlers
    - [x] Detailed explanatory output showing capability negotiation
    - [x] Graceful error messages if PyTorch/CUDA unavailable
    - [x] Tests: Import/architecture validated (visual testing requires CUDA)

**Dependencies**: Phase 3.3, PyTorch (optional - conditional import)

**Time taken**: 1 session

**Files created**:
- `packages/streamlib/src/streamlib/handlers/blur_gpu.py` (215 lines)
- `examples/demo_gpu.py` (137 lines)

**Files updated**:
- `packages/streamlib/src/streamlib/handlers/__init__.py` - Conditional GPU exports
- `packages/streamlib/src/streamlib/__init__.py` - Conditional GPU exports

**Validation**:
- ✅ BlurFilterGPU implements GPU-only capability
- ✅ Conditional imports work (no crash without PyTorch)
- ✅ Transfer handlers from Phase 3.1 ready for GPU↔CPU
- ✅ Demo explains capability negotiation clearly
- ⚠️  Visual GPU testing requires CUDA-capable hardware

**Key Innovation**:
Runtime automatically inserts transfer handlers when connecting handlers with incompatible capabilities. No manual memory management required!

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
