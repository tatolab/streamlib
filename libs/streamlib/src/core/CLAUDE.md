# Core - Platform-Agnostic Layer

**This is what you should use!**

## ⚠️ CRITICAL: Always Use the StreamProcessor Macro

**When creating new processors, ALWAYS use `#[derive(StreamProcessor)]`:**

```rust
use streamlib::{StreamInput, StreamOutput, VideoFrame};

#[derive(StreamProcessor)]
struct MyProcessor {
    #[input()]
    video_in: StreamInput<VideoFrame>,

    #[output()]
    video_out: StreamOutput<VideoFrame>,

    config_field: f32,  // Auto-becomes config
}

impl MyProcessor {
    fn process(&mut self) -> Result<()> {
        // Your logic here
        Ok(())
    }
}
```

The macro auto-generates 90+ lines of boilerplate including:
- Config struct
- `from_config()` constructor
- `descriptor()` with type-safe schemas
- Descriptions, tags, examples
- Full MCP compatibility

See `CLAUDE.md` (root) and `libs/streamlib-macros/CLAUDE.md` for details.

## What This Is

The **core** module contains platform-agnostic traits, types, and runtime logic that work everywhere:

- **Traits**: `CameraProcessor`, `AudioCaptureProcessor`, `AudioOutputProcessor`, `AudioEffectProcessor`
- **Types**: `VideoFrame`, `AudioFrame`, `StreamProcessor`, `StreamRuntime`
- **Schemas**: Type definitions for AI/MCP discoverability
- **Runtime**: Clock, tick broadcaster, processor lifecycle
- **Ports**: Type-safe connection system
- **Macros**: `#[derive(StreamProcessor)]` for automatic trait implementation

## The Key Design Pattern

**You write code against traits, not implementations:**

```rust
use streamlib::{CameraProcessor, AudioCaptureProcessor, StreamRuntime};

// This code works on macOS, Linux, Windows - no changes needed!
let camera = CameraProcessor::new(None)?;
let mic = AudioCaptureProcessor::new(None, 48000, 2)?;

let mut runtime = StreamRuntime::new();
runtime.add_processor(Box::new(camera));
runtime.add_processor(Box::new(mic));
```

At compile time, the correct platform implementation is automatically selected:
- **macOS/iOS** → `apple::AppleCameraProcessor`
- **Linux** → `linux::LinuxCameraProcessor` (future)
- **Windows** → `windows::WindowsCameraProcessor` (future)

## Module Breakdown

### `processors/` - Processor Traits

**These are what users import:**

```rust
use streamlib::{
    CameraProcessor,           // Video capture from cameras
    DisplayProcessor,          // Video output to windows
    AudioCaptureProcessor,     // Audio capture from microphones
    AudioOutputProcessor,      // Audio output to speakers
    AudioEffectProcessor,      // Audio effects (base trait)
    ClapEffectProcessor,       // CLAP plugin hosting
};
```

Each trait defines:
- Constructor: `new(...)` with platform-appropriate args
- Device enumeration: `list_devices()`
- Port structure: What inputs/outputs the processor has
- Processing behavior: How it transforms data

### `messages.rs` - Data Types

The fundamental data types that flow through processors:

```rust
pub struct VideoFrame {
    pub texture: Texture,      // GPU texture (zero-copy)
    pub timestamp_ns: i64,
    pub frame_number: u64,
    // ... metadata
}

pub struct AudioFrame {
    pub samples: Vec<f32>,     // CPU buffer (interleaved)
    pub gpu_buffer: Option<Buffer>,  // Optional GPU buffer
    pub timestamp_ns: i64,
    pub sample_rate: u32,
    pub channels: u32,
    // ... metadata
}
```

### `runtime.rs` - Execution Engine

The runtime manages:
- **Clock**: Generates ticks at fixed rate (e.g., 60 FPS)
- **Broadcaster**: Distributes ticks to all processors
- **Processor Lifecycle**: Start, stop, connect
- **Audio Config**: Shared sample rate/buffer size

### `ports.rs` - Type-Safe Connections

```rust
pub struct StreamOutput<T: PortMessage> {
    buffer: Arc<RingBuffer<T>>,
}

pub struct StreamInput<T: PortMessage> {
    buffer: Option<Arc<RingBuffer<T>>>,
}

// Type-safe connection
runtime.connect(&mut camera.ports.output.video, &mut display.ports.input.video)?;
```

This prevents runtime errors like connecting audio to video.

### `schema.rs` - AI/MCP Discoverability

Processors describe themselves for AI agents:

```rust
impl StreamProcessor for CameraProcessor {
    fn descriptor() -> Option<ProcessorDescriptor> {
        Some(
            ProcessorDescriptor::new("CameraProcessor", "Captures video from cameras")
                .with_output(PortDescriptor::new("video", SCHEMA_VIDEO_FRAME, ...))
                .with_tags(vec!["source", "video", "camera"])
        )
    }
}
```

This enables:
- MCP servers to discover processors
- AI agents to build pipelines
- Runtime introspection

### `registry.rs` - Processor Discovery

```rust
// Auto-registration via macro
register_processor_type!(CameraProcessor);

// Discovery
let processors = list_processors();
let camera_proc = processors.iter()
    .find(|p| p.tags.contains(&"camera"))
    .unwrap();
```

## The CLAP Philosophy

**CLAP is a core dependency, not optional:**

Just as `wgpu` is the standard way to do GPU operations in streamlib, **CLAP is the standard way to process audio**.

```rust
use streamlib::ClapEffectProcessor;

let reverb = ClapEffectProcessor::load("/path/to/reverb.clap")?;
reverb.activate(48000, 2048)?;
```

CLAP is to audio what WGSL shaders are to video - the standardized, composable processing layer.

## Platform Routing

**How does `CameraProcessor::new()` know which implementation to use?**

1. User imports `streamlib::CameraProcessor` (the trait)
2. Top-level `streamlib` crate re-exports platform-specific type:
   ```rust
   #[cfg(target_os = "macos")]
   pub use apple::AppleCameraProcessor as CameraProcessor;

   #[cfg(target_os = "linux")]
   pub use linux::LinuxCameraProcessor as CameraProcessor;
   ```
3. User calls `CameraProcessor::new()` → automatically routed to correct platform

**Result**: Same code compiles on all platforms, zero runtime overhead.

## When To Use What

| Use Case | Import From |
|----------|-------------|
| **Building pipelines** | `use streamlib::{CameraProcessor, AudioCaptureProcessor, ...}` |
| **Processing data** | `use streamlib::{VideoFrame, AudioFrame, ...}` |
| **Runtime management** | `use streamlib::StreamRuntime` |
| **AI/MCP integration** | `use streamlib::{ProcessorDescriptor, list_processors}` |
| **Platform internals** | ❌ Don't! Use traits above |

## Port Exposure Pattern

**All processors must expose their ports publicly:**

```rust
pub struct CameraOutputPorts {
    pub video: StreamOutput<VideoFrame>,
}

pub struct CameraPorts {
    pub output: CameraOutputPorts,
}

pub struct CameraProcessor {
    pub ports: CameraPorts,  // PUBLIC!
    // ... private implementation details
}
```

This enables type-safe connections:

```rust
runtime.connect(
    &mut camera.ports.output.video,
    &mut display.ports.input.video
)?;
```

**If ports aren't public, the processor can't be connected!**

## Related Files

- `../apple/` - macOS/iOS implementations (internal)
- `../linux/` - Linux implementations (future, internal)
- `../windows/` - Windows implementations (future, internal)
- `../../lib.rs` - Public API facade
- `/CLAUDE.md` - Repository-wide architecture
