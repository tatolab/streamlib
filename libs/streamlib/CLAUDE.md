# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Working Directory Context

This is the **core streamlib library crate** (`libs/streamlib`). The repository root is two directories up (`../../`).

- **Examples**: Located at `../../examples/` (e.g., `camera-display`, `microphone-reverb-speaker`)
- **Root Cargo.toml**: Workspace root at `../../Cargo.toml`
- **Documentation**: Root docs at `../../docs/`

## Build and Test Commands

All commands should be run from **this directory** (`libs/streamlib`):

```bash
# Build the library only
cargo build --lib

# Build with specific features
cargo build --lib --features python
cargo build --lib --features mcp
cargo build --lib --features debug-overlay

# Run tests
cargo test

# Run specific test
cargo test test_name

# Run tests with output
cargo test -- --nocapture

# Run library tests only (excludes integration tests)
cargo test --lib

# Run benchmarks
cargo bench

# Run specific benchmark
cargo bench connection_bench

# Linting
cargo clippy --no-deps
cargo fmt

# Generate documentation
cargo doc --open --no-deps
```

### Running Examples

Examples must be run from the **repository root**:

```bash
cd ../../
cargo run -p camera-display
cargo run -p microphone-reverb-speaker
RUST_LOG=debug cargo run -p camera-audio-recorder
```

### Building Python Bindings

From this directory:

```bash
# Build and install in development mode
maturin develop --features python

# Build release wheel
maturin build --features python --release
```

### Running MCP Server

From repository root:

```bash
cd ../../
cargo run --bin streamlib-mcp --features mcp
```

## Development Philosophy: No Hacks, Ask Questions

**CRITICAL RULE**: When you encounter a problem that would require a workaround, hack, or `unimplemented!()`:

1. ⛔ **STOP** - Do not implement the hack
2. 🤔 **ANALYZE** - Understand why the problem exists
3. ❓ **ASK** - Present the problem and ask for guidance
4. ✅ **DEFER** - Wait for architectural direction from the user

**Examples of prohibited patterns**:
- `unimplemented!()` or `todo!()` in library code (tests are OK)
- "Temporary" workarounds that bypass type safety
- Compatibility shims for "old code" in new implementations
- Methods that exist but do nothing (`fn foo() { /* no-op */ }`)

**Why this matters**: Hacks accumulate and create technical debt. The user has deep architectural knowledge and can provide better solutions than quick fixes.

**What to do instead**:
```
"I've hit a problem where X needs Y, but Z is preventing it. I see three options:
1. [Option A with tradeoffs]
2. [Option B with tradeoffs]
3. [Option C with tradeoffs]

Which approach aligns with the Phase 0.5 architecture?"
```

## Architectural Truth: Runtime and Graph are in Control

**CRITICAL UNDERSTANDING**: In StreamLib's architecture:

### The Runtime is the Orchestrator
- **Runtime** creates and manages processor lifecycle
- **ConnectionManager** creates connections (OwnedProducer/OwnedConsumer pairs)
- **Graph representation** is the single source of truth (directed acyclic graph)
- **Runtime** wires connections to processor ports via `wire_*` methods

### Processors are Declarative
- **Processors declare** inputs/outputs via `#[input]` / `#[output]` attributes
- **Processors implement** business logic in `process()` method
- **Processors DO NOT** manage their own connections
- **Processors DO NOT** control wiring or lifecycle

### Dynamic Graph Operations
The system must support:
- ✅ Connecting/disconnecting while runtime is **running**
- ✅ Connecting/disconnecting while runtime is **paused**
- ✅ Connecting/disconnecting while runtime is **stopped**
- ✅ Graph optimization (runtime analyzes DAG, reorders, parallelizes)
- ✅ Runtime has full control over execution strategy

### What This Means for Code
```rust
// ❌ WRONG - Processor managing connections
impl MyProcessor {
    pub fn add_connection(&mut self, ...) {
        self.connections.push(...);  // Processor controls state
    }
}

// ✅ RIGHT - Runtime manages connections, processor just has ports
#[derive(StreamProcessor)]
struct MyProcessor {
    #[output]
    video_out: StreamOutput<VideoFrame>,  // Port declaration only
}

// Runtime does the wiring:
let (producer, consumer) = connection_manager.create_connection(...)?;
processor.video_out.wire_producer(connection_id, producer, wakeup)?;
```

### DynStreamElement is for Runtime Control
`DynStreamElement` trait methods should only include what the **runtime needs** to:
- Control lifecycle (`setup`, `teardown`, `process`)
- Query metadata (`name`, `descriptor`, `port_types`)
- Set wakeup channels for event-driven scheduling
- Access type-erased processor (`as_any_mut` for downcasting)

`DynStreamElement` should **NOT** include:
- Connection management (`add_connection`, `remove_connection`) - that's ConnectionManager's job
- Graph manipulation - that's Runtime's job
- Execution strategy - that's Runtime's job

## Type Safety and Internal API Design Principles

**CRITICAL**: These principles MUST be followed for all internal APIs, runtime code, and generated code.

### 1. Use the Type System - Never Dumb Down Internal APIs

**Internal methods should be maximally specific and type-safe**. Don't accept generic types like `String` or `&str` when you mean a specific domain concept.

✅ **Good - Type-safe**:
```rust
pub fn add_connection(
    &mut self,
    connection_id: ConnectionId,  // Validated, domain-specific type
    producer: OwnedProducer<T>,
    wakeup: Sender<WakeupEvent>,
) -> Result<(), StreamError>
```

❌ **Bad - Stringly typed**:
```rust
pub fn add_connection(
    &mut self,
    connection_id: &str,  // Could be anything! No validation!
    producer: Box<dyn Any>,
    wakeup: Sender<WakeupEvent>,
) -> bool  // What does false mean?
```

### 2. Return Result<T, E>, Never Bool for Fallible Operations

**Errors must be descriptive**. Bool returns are lazy and lose critical debugging information.

✅ **Good - Descriptive errors**:
```rust
pub fn remove_connection(&mut self, id: &ConnectionId) -> Result<(), StreamError> {
    self.connections.iter().position(|c| c.id() == id)
        .ok_or_else(|| StreamError::ConnectionNotFound(id.to_string()))?;
    // ... remove logic
    Ok(())
}
```

❌ **Bad - Mystery bool**:
```rust
pub fn remove_connection(&mut self, id: &ConnectionId) -> bool {
    // What does false mean? Not found? Permission denied? Network error?
    if let Some(idx) = self.connections.iter().position(|c| c.id() == id) {
        // ... remove logic
        true
    } else {
        false  // User has no idea why it failed
    }
}
```

### 3. Validated Newtypes with Type Guards

**Use the newtype pattern** for domain-specific IDs, handles, and validated strings. Provide `from_string()` as a "type guard" for validation.

✅ **Good - Validated newtype**:
```rust
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ConnectionId(String);

impl ConnectionId {
    /// Type guard: validates and creates ConnectionId
    pub fn from_string(s: impl Into<String>) -> Result<Self, ConnectionIdError> {
        let s = s.into();
        if s.is_empty() {
            return Err(ConnectionIdError::Empty);
        }
        // ... more validation
        Ok(Self(s))
    }

    /// Internal use only - bypasses validation
    pub(crate) fn new_unchecked(s: impl Into<String>) -> Self {
        Self(s.into())
    }
}

// Implement Deref for ergonomic &str comparisons
impl Deref for ConnectionId {
    type Target = str;
    fn deref(&self) -> &Self::Target { &self.0 }
}
```

❌ **Bad - Type alias with no validation**:
```rust
pub type ConnectionId = String;  // Can be any string, no validation!
```

### 4. No Implicit Conversions - Explicit is Better

**Don't accept generic `Into<String>` or `AsRef<str>` in internal APIs**. Make callers explicitly validate/convert.

✅ **Good - Explicit validation required**:
```rust
// Caller must explicitly validate:
let conn_id = ConnectionId::from_string("my_id")?;  // Validation happens here
port.add_connection(conn_id, ...)?;  // Type-safe, validated
```

❌ **Bad - Accepts anything**:
```rust
// Accepts any string without validation:
pub fn add_connection(&mut self, id: impl Into<String>, ...) {
    let id = id.into();  // No validation! Could be empty, malformed, etc.
    // ...
}
```

### 5. No Legacy Compatibility Hacks in Internal Code

**Delete old code, don't support it**. If the old API is wrong, make it a compile error.

✅ **Good - Clean break**:
```rust
// Old code deleted. New code won't compile until fixed.
pub fn add_connection(connection_id: ConnectionId, ...) -> Result<()>
```

❌ **Bad - Backwards compatibility mess**:
```rust
// Supporting both old and new patterns
pub fn add_connection(&mut self, id: impl Into<String>, ...) -> Result<()> {
    let temp_id = ConnectionId::new_unchecked(id.into());  // Bypasses validation!
    // ...
}
```

### 6. Lock-Free is Non-Negotiable for Hot Paths

**Never use `Arc<Mutex<T>>` in data flow paths**. Only atomic operations (`rtrb::Producer`, `AtomicUsize`, etc.).

✅ **Good - Lock-free**:
```rust
pub struct OwnedProducer<T> {
    inner: Producer<T>,  // rtrb lock-free ring buffer
    cached_size: Arc<AtomicUsize>,  // Only atomic operations
}

impl<T> OwnedProducer<T> {
    pub fn write(&mut self, data: T) {
        match self.inner.push(data) {  // Lock-free atomic operation
            Ok(()) => self.cached_size.fetch_add(1, Ordering::Relaxed),
            Err(_) => { /* drop data */ }
        }
    }
}
```

❌ **Bad - Locks in hot path**:
```rust
pub struct Producer<T> {
    inner: Arc<Mutex<rtrb::Producer<T>>>,  // KILLS PERFORMANCE!
}

impl<T> Producer<T> {
    pub fn write(&mut self, data: T) {
        let mut inner = self.inner.lock();  // Contention!
        inner.push(data).ok();
    }
}
```

### 7. Owned vs Borrowed Parameters

**Guidelines**:
- **Take ownership** (`ConnectionId`) when the value is stored/moved
- **Take reference** (`&ConnectionId`) when just reading/comparing
- **Take `&mut`** when modifying in place

```rust
// Ownership transferred - ConnectionId stored in port
pub fn add_connection(&mut self, id: ConnectionId, ...) -> Result<()> {
    self.connections.push(Connection { id, ... });  // id moved here
    Ok(())
}

// Just comparing - no ownership needed
pub fn remove_connection(&mut self, id: &ConnectionId) -> Result<()> {
    let idx = self.connections.iter().position(|c| c.id() == id)?;
    // ...
}
```

### 8. Convenience Helpers are Separate from Core

**Core internal APIs are strict**. Create separate convenience functions/methods for ergonomics.

```rust
// Core API - strict, type-safe
impl StreamOutput<T> {
    pub fn add_connection(
        &mut self,
        connection_id: ConnectionId,  // Must be validated
        producer: OwnedProducer<T>,
        wakeup: Sender<WakeupEvent>,
    ) -> Result<()> { ... }
}

// Convenience wrapper - validates and delegates
impl StreamOutput<T> {
    pub fn try_add_connection_from_string(
        &mut self,
        id_str: &str,  // Accepts string for convenience
        producer: OwnedProducer<T>,
        wakeup: Sender<WakeupEvent>,
    ) -> Result<()> {
        let connection_id = ConnectionId::from_string(id_str)?;  // Validate
        self.add_connection(connection_id, producer, wakeup)  // Delegate to core
    }
}
```

**Summary**: Internal APIs enforce correctness through types. Convenience layers provide ergonomics.

## Code Architecture (libs/streamlib Specific)

### Module Structure

```
src/
├── lib.rs                    # Public API surface, platform re-exports
├── runtime.rs                # High-level StreamRuntime wrapper
├── core/                     # Platform-agnostic core (99% of code lives here)
│   ├── mod.rs               # Core exports
│   ├── traits/              # StreamProcessor, StreamElement, DynStreamElement
│   ├── processors/          # Cross-platform processor implementations
│   ├── bus/                 # Lock-free connections (OwnedProducer/Consumer)
│   ├── frames/              # VideoFrame, AudioFrame<N>, DataFrame
│   ├── runtime.rs           # Core runtime (thread spawning, lifecycle)
│   ├── context/             # GpuContext, RuntimeContext
│   ├── sync.rs              # A/V synchronization utilities
│   ├── scheduling/          # SchedulingMode (Loop/Push/Pull), priority
│   ├── pubsub/              # Global EVENT_BUS, parallel pub/sub
│   ├── clap/                # CLAP audio plugin hosting
│   ├── schema.rs            # Processor metadata, type system
│   ├── registry.rs          # Global processor registry
│   └── topology.rs          # Graph analysis tools
├── apple/                   # macOS/iOS implementations (conditionally compiled)
│   ├── processors/          # AVFoundation-based processors
│   │   ├── camera.rs       # AVCaptureDevice wrapper
│   │   ├── display.rs      # CAMetalLayer rendering
│   │   ├── audio_output.rs # CoreAudio output
│   │   ├── audio_capture.rs # CoreAudio input
│   │   ├── mp4_writer.rs   # AVAssetWriter wrapper
│   │   └── webrtc.rs       # VideoToolbox H.264 encoder (WIP)
│   ├── metal_bridge.rs     # Metal ↔ wgpu texture sharing (IOSurface)
│   └── permissions.rs      # macOS permission prompts
├── mcp/                     # Model Context Protocol server (feature: mcp)
├── python/                  # PyO3 bindings (feature: python)
└── bin/
    └── streamlib-mcp.rs     # MCP server binary
```

### Key Design Patterns

#### 1. Core vs Platform-Specific Code

**Rule**: All platform-agnostic code goes in `core/`. Platform-specific implementations go in platform modules (`apple/`).

**Pattern for platform-specific processors**:

```rust
// core/processors/camera.rs - Trait definition (if needed for cross-platform)
pub trait CameraConfig { ... }

// apple/processors/camera.rs - Implementation
pub struct AppleCameraProcessor { ... }

// lib.rs - Conditional re-export with unified name
#[cfg(any(target_os = "macos", target_os = "ios"))]
pub use apple::AppleCameraProcessor as CameraProcessor;
```

**DO NOT** use `#[cfg(target_os = "macos")]` inside `apple/processors/` files - they're already conditionally compiled at the module level.

#### 2. Main Thread Dispatch (macOS/iOS Critical Pattern)

**Problem**: Apple frameworks (AVFoundation, VideoToolbox, AVAssetWriter) **require** main thread execution.

**Solution**: `RuntimeContext` provides main thread dispatch:

```rust
// In processor's process() or setup()
let ctx = self.ctx.as_ref().unwrap();

// Blocking call (waits for result)
let result = ctx.run_on_main_blocking(|| {
    // This closure runs on main thread with active NSRunLoop
    unsafe {
        VTCompressionSessionCreate(...)
    }
})?;

// Async call (returns immediately)
ctx.run_on_main_async(|| {
    writer_input.append(sample_buffer)
});
```

**When to use**:
- Creating/configuring `AVCaptureDevice`, `AVAssetWriter`, `VTCompressionSession`
- Appending samples to `AVAssetWriterInput`
- Any CoreFoundation/CoreMedia/VideoToolbox operations
- Creating Metal textures that interact with AVFoundation

**NEVER** call `run_on_main_blocking()` from the main thread - it will deadlock.

See: `../../docs/main_thread_dispatch.md`

#### 3. Processor Lifecycle and Threading

Each processor gets its own thread with a scheduling mode:

**Loop Mode** (default):
```rust
SchedulingConfig {
    mode: SchedulingMode::Loop,
    priority: ThreadPriority::UserInteractive,
}
```
- Tight loop with 10μs sleep
- Checks shutdown signal each iteration
- Use for: Transformers, filters

**Push Mode**:
```rust
SchedulingConfig {
    mode: SchedulingMode::Push,
    priority: ThreadPriority::UserInteractive,
}
```
- Event-driven, woken when input data arrives
- Use for: Sinks (DisplayProcessor, AudioOutputProcessor)

**Pull Mode**:
```rust
SchedulingConfig {
    mode: SchedulingMode::Pull,
    priority: ThreadPriority::RealTime,
}
```
- Processor manages its own callbacks
- Runtime only waits for shutdown
- Use for: Sources with platform callbacks (CameraProcessor, AudioCaptureProcessor on macOS)

**Lifecycle phases**:
1. Construction: `new()` or `from_config()`
2. Setup: `__generated_setup(ctx)` (macro-generated, calls user's `setup()` if present)
3. Processing: `process()` called in loop (Loop/Push) or by processor (Pull)
4. Teardown: `__generated_teardown()` (macro-generated, drops resources)

#### 4. Lock-Free Bus Architecture (Phase 2)

**Current implementation** (used by all new code):
- `OwnedProducer<T>` / `OwnedConsumer<T>` (owned by each processor)
- Lock-free via `rtrb` ring buffer (atomic operations only)
- Writer never blocks - drops old data if buffer full (acceptable for real-time)
- Reader gets latest frame via `read_latest()` or sequential via `read()`

**Pattern in processor**:
```rust
#[derive(StreamProcessor)]
struct MyProcessor {
    #[input]
    video_in: StreamInput<VideoFrame>,  // Wraps OwnedConsumer
    #[output]
    video_out: StreamOutput<VideoFrame>,  // Wraps OwnedProducer
}

impl MyProcessor {
    fn process(&mut self) -> Result<()> {
        // Non-blocking read (returns None if no data)
        if let Some(frame) = self.video_in.read_latest() {
            let processed = self.transform(frame)?;

            // Non-blocking write (drops if full)
            self.video_out.write(processed);
        }
        Ok(())
    }
}
```

**Deprecated Phase 1** (DO NOT use):
- `ProcessorConnection<T>` with `Arc<Mutex<Producer/Consumer>>`
- Being phased out

#### 5. Timestamp System (Critical for A/V Sync)

**All timestamps are monotonic nanoseconds** (`i64`) from `MediaClock::now()`:
- macOS: `mach_absolute_time()` converted to nanoseconds
- Other platforms: `Instant::now()` from epoch

**Usage**:
```rust
use streamlib::core::media_clock::MediaClock;

let timestamp_ns = MediaClock::now().as_nanos() as i64;
let frame = VideoFrame::new(texture, format, timestamp_ns, frame_num, width, height);
```

**A/V synchronization utilities** (`core/sync.rs`):
```rust
use streamlib::core::{video_audio_delta_ms, are_synchronized};

let delta = video_audio_delta_ms(&video_frame, &audio_frame);
if are_synchronized(&video_frame, &audio_frame, 16.6) {
    // Within ~1 frame @ 60fps
}
```

**Converting timestamps**:
```rust
// Nanoseconds → seconds
let seconds = timestamp_ns as f64 / 1_000_000_000.0;

// Nanoseconds → RTP timestamp (90kHz for H.264 video)
let rtp_ts = (timestamp_ns as i128 * 90_000 / 1_000_000_000) as u32;

// Nanoseconds → CMTime (for AVFoundation)
let cm_time = CMTime::new(timestamp_ns, 1_000_000_000);
```

**NEVER use `SystemTime::now()`** - it's not monotonic (affected by clock adjustments).

#### 6. GPU Context and Texture Sharing

**GpuContext** is shared across all processors:
```rust
fn process(&mut self) -> Result<()> {
    let ctx = self.ctx.as_ref().unwrap();
    let gpu = ctx.gpu();
    let device = gpu.device();
    let queue = gpu.queue();

    // Use device/queue for GPU operations
}
```

**VideoFrame textures** are `Arc<wgpu::Texture>` - cheap to clone, zero-copy sharing.

**Metal interop** (macOS only):
```rust
use streamlib::apple::WgpuBridge;

// Import Metal texture as wgpu texture
let wgpu_texture = WgpuBridge::import_metal_texture(
    &metal_texture,
    &device,
    width,
    height,
    wgpu::TextureFormat::Bgra8Unorm,
)?;
```

Uses IOSurface for zero-copy sharing between Metal and wgpu.

#### 7. Event Bus (Pub/Sub System)

**Global singleton** for runtime monitoring:
```rust
use streamlib::core::EVENT_BUS;

// Subscribe to events
let listener = EVENT_BUS.subscribe("runtime:global", |event| {
    match event.as_ref() {
        Event::Runtime(RuntimeEvent::ProcessorAdded { processor_id, .. }) => {
            tracing::info!("Processor added: {}", processor_id);
        }
        _ => {}
    }
});

// Keep listener alive (weak references used)
self._event_listener = listener;
```

**Topics**:
- `runtime:global` - Runtime lifecycle events
- `processor:{id}` - Per-processor events
- Custom topics

**Characteristics**:
- Lock-free via DashMap (concurrent HashMap)
- Parallel dispatch via Rayon
- Fire-and-forget (busy listeners skipped)
- Auto-cleanup via weak references

## Common Development Tasks in This Crate

### Adding a New Core Processor

1. **Create file**: `src/core/processors/my_processor.rs`
2. **Implement using macro**:
```rust
use crate::core::*;

#[derive(StreamProcessor)]
#[processor(
    description = "Does something cool",
    tags = "video,transform"
)]
pub struct MyProcessor {
    #[input(description = "Input video")]
    video_in: StreamInput<VideoFrame>,

    #[output(description = "Output video")]
    video_out: StreamOutput<VideoFrame>,

    #[config]
    config: MyProcessorConfig,
}

#[derive(Serialize, Deserialize, Default)]
pub struct MyProcessorConfig {
    pub strength: f32,
}

impl MyProcessor {
    fn process(&mut self) -> Result<()> {
        if let Some(frame) = self.video_in.read_latest() {
            let processed = self.transform(frame)?;
            self.video_out.write(processed);
        }
        Ok(())
    }

    fn transform(&mut self, frame: VideoFrame) -> Result<VideoFrame> {
        // Your processing logic here
        Ok(frame)
    }
}
```
3. **Export in `core/processors/mod.rs`**:
```rust
mod my_processor;
pub use my_processor::{MyProcessor, MyProcessorConfig};
```
4. **Re-export in `lib.rs`** public API:
```rust
pub use core::{
    MyProcessor, MyProcessorConfig,
    // ... other exports
};
```

### Adding an Apple-Specific Processor

1. **Create file**: `src/apple/processors/my_processor.rs`
2. **Implement using Apple frameworks**:
```rust
use objc2_av_foundation::*;
use crate::core::*;

#[derive(StreamProcessor)]
pub struct AppleMyProcessor {
    #[output]
    output: StreamOutput<VideoFrame>,

    ctx: Option<Arc<RuntimeContext>>,
    av_object: Option<Id<AVSomething>>,
}

impl AppleMyProcessor {
    fn setup(&mut self, ctx: Arc<RuntimeContext>) -> Result<()> {
        self.ctx = Some(ctx.clone());

        // CRITICAL: Use main thread dispatch for Apple APIs
        let av_object = ctx.run_on_main_blocking(|| {
            unsafe {
                AVSomething::new()
            }
        })?;

        self.av_object = Some(av_object);
        Ok(())
    }

    fn process(&mut self) -> Result<()> {
        // Processing logic
        Ok(())
    }

    fn scheduling_config(&self) -> SchedulingConfig {
        SchedulingConfig {
            mode: SchedulingMode::Pull,  // If using callbacks
            priority: ThreadPriority::RealTime,
        }
    }
}
```
3. **Export in `apple/processors/mod.rs`**:
```rust
mod my_processor;
pub use my_processor::AppleMyProcessor;
```
4. **Re-export in `lib.rs`** with platform gate:
```rust
#[cfg(any(target_os = "macos", target_os = "ios"))]
pub use apple::AppleMyProcessor as MyProcessor;
```

### Working with VideoToolbox (H.264 Encoding)

**Current work**: `src/apple/processors/webrtc.rs`

**Pattern - wgpu Texture → CVPixelBuffer → H.264**:
```rust
// 1. Copy wgpu texture to CPU-accessible buffer
let staging_buffer = device.create_buffer(&BufferDescriptor {
    size: buffer_size,
    usage: BufferUsages::COPY_DST | BufferUsages::MAP_READ,
    mapped_at_creation: false,
});

encoder.copy_texture_to_buffer(
    texture.as_image_copy(),
    ImageCopyBuffer { buffer: &staging_buffer, ... },
    texture_size,
);

queue.submit([encoder.finish()]);

// 2. Map buffer and get RGBA data
let (tx, rx) = std::sync::mpsc::channel();
buffer_slice.map_async(MapMode::Read, move |result| {
    tx.send(result).unwrap();
});
device.poll(wgpu::Maintain::Wait);
rx.recv().unwrap()?;

let rgba_data = buffer_slice.get_mapped_range();

// 3. Convert RGBA → NV12 (YUV 4:2:0 for H.264)
use yuv::*;
let mut yuv_image = YuvImage::alloc_nv12(width, height);
rgba_to_yuv_nv12(&mut yuv_image, &rgba_data, stride, YuvRange::Limited);

// 4. Create CVPixelBuffer wrapping YUV data
let pixel_buffer = ctx.run_on_main_blocking(|| unsafe {
    CVPixelBufferCreate(
        None,
        width as usize,
        height as usize,
        kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange,
        attrs,
        &mut pixel_buffer,
    )
})?;

// 5. Encode with VideoToolbox (MUST run on main thread)
ctx.run_on_main_blocking(|| unsafe {
    VTCompressionSessionEncodeFrame(
        session,
        pixel_buffer,
        presentation_time,
        duration,
        None,
        source_frame_refcon,
        None,
    )
})?;
```

**Key points**:
- ALL VideoToolbox operations must use `run_on_main_blocking()`
- Use `yuv` crate for RGBA → NV12 conversion (SIMD-optimized)
- NV12 format: Y plane (full res) + interleaved UV plane (half res)
- RTP timestamp calculation: `(timestamp_ns * 90_000) / 1_000_000_000`

### Adding Tests

**Unit tests** in same file:
```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_my_processor() {
        let mut processor = MyProcessor::from_config(Default::default()).unwrap();
        // Test logic
    }
}
```

**Integration tests** in `tests/`:
```rust
// tests/my_integration_test.rs
use streamlib::*;

#[test]
fn test_pipeline() {
    let mut runtime = StreamRuntime::new();
    // Build and test pipeline
}
```

**Platform-specific tests**:
```rust
#[cfg(target_os = "macos")]
#[test]
fn test_apple_processor() {
    // macOS-only test
}
```

## Project-Specific Conventions

### Error Handling
- Use `StreamError` enum from `core/error.rs`
- Return `Result<T>` from all fallible operations
- Prefer `?` operator over `.unwrap()` in library code
- `.unwrap()` acceptable in examples and tests

### Port Naming
- Video: `"video"`, `"video_in"`, `"video_out"`
- Audio: `"audio"`, `"audio_in"`, `"audio_out"`
- Audio frame types: `AudioFrame<1>` (mono), `AudioFrame<2>` (stereo), `AudioFrame<4>` (quad), etc.

### Logging
- Use `tracing` crate, not `println!`
- Levels: `error!`, `warn!`, `info!`, `debug!`, `trace!`
- Example: `tracing::info!("Processing frame {}", frame_num);`

### Feature Flags in Code
```rust
#[cfg(feature = "python")]
mod python;

#[cfg(feature = "mcp")]
mod mcp;

#[cfg(feature = "debug-overlay")]
pub use processors::PerformanceOverlayProcessor;
```

## Known Issues and Gotchas

### 1. Main Thread Deadlock
**NEVER** call `run_on_main_blocking()` from the main thread - it will deadlock waiting for itself.

### 2. AudioFrame Channel Count is Generic
Must specify exact channel count at compile time:
```rust
StreamInput<AudioFrame<2>>  // Stereo - OK
StreamInput<AudioFrame<N>>  // Generic N - ERROR
```

### 3. Metal/wgpu Texture Format Mismatch
When importing Metal textures:
- Metal `MTLPixelFormatBGRA8Unorm` → wgpu `TextureFormat::Bgra8Unorm`
- Camera output is BGRA, not RGBA

### 4. Lock-Free Bus Drops Old Data
Phase 2 bus drops data when full - by design for real-time processing. If you need every frame, increase buffer size or use backpressure at application level.

### 5. Event Listener Lifetime
Event bus uses weak references. Store listener handle to keep it alive:
```rust
struct MyProcessor {
    _event_listener: EventListener,  // Underscore prefix = "used for lifetime"
}
```

### 6. Timestamps Must Be Monotonic
Never use `SystemTime::now()` - use `MediaClock::now()` which is monotonic and unaffected by clock adjustments.

### 7. Circular Dependencies in Workspace
If build fails with circular dependency errors, build the library first:
```bash
cargo build --lib
cargo build -p example-name
```

## Performance Considerations

1. **Avoid allocations in `process()`**: Reuse buffers, pre-allocate in `setup()`
2. **Batch GPU operations**: Group texture copies, use command encoders efficiently
3. **Main thread dispatch overhead**: Keep closures fast (microseconds, not milliseconds)
4. **Audio buffer sizes**: Use powers of 2 for optimal SIMD performance
5. **Lock-free semantics**: `read_latest()` is fastest (discards stale frames)

## Documentation Standards

- All public types/functions require doc comments (`///`)
- Include usage examples for complex APIs
- Mark `unsafe` code with safety invariants
- Document platform-specific behavior ("macOS only", "Requires main thread")

## Key Files to Reference

### Core Architecture
- `src/core/traits/processor.rs` - StreamProcessor trait definition
- `src/core/runtime.rs` - Core runtime implementation
- `src/core/bus/connection.rs` - Lock-free connection implementation
- `src/core/scheduling/mode.rs` - Scheduling modes (Loop/Push/Pull)
- `src/core/sync.rs` - A/V synchronization utilities
- `src/core/media_clock.rs` - Monotonic timestamp system

### Apple-Specific
- `src/apple/metal_bridge.rs` - Metal ↔ wgpu interop via IOSurface
- `src/apple/processors/camera.rs` - AVFoundation camera example
- `src/apple/processors/webrtc.rs` - VideoToolbox H.264 encoding (WIP)

### Build Configuration
- `Cargo.toml` - Dependencies and features
- `build.rs` - Build-time Metal framework linking
- `pyproject.toml` - Python package configuration (maturin)

## Additional Resources

- **Main thread dispatch guide**: `../../docs/main_thread_dispatch.md`
- **WebRTC implementation plan**: `../../WEBRTC_IMPLEMENTATION_PLAN.md`
- **Examples** (best way to learn): `../../examples/`
  - `camera-display` - Basic video pipeline
  - `microphone-reverb-speaker` - Audio with CLAP plugin
  - `camera-audio-recorder` - MP4 file writing
  - `news-cast` - Complex multi-source composition
