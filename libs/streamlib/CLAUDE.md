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
