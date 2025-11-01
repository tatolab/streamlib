# streamlib - The Unified Crate

This is the **user-facing crate** that provides a platform-agnostic API.

## ⚠️ Creating Processors: Use the Macro

**ALWAYS use `#[derive(StreamProcessor)]` for new processors:**

```rust
use streamlib::{StreamInput, StreamOutput, VideoFrame};

#[derive(StreamProcessor)]
struct MyProcessor {
    #[input()]
    input: StreamInput<VideoFrame>,

    #[output()]
    output: StreamOutput<VideoFrame>,

    // Config fields
    strength: f32,
}

impl MyProcessor {
    fn process(&mut self) -> Result<()> {
        // Your logic
        Ok(())
    }
}
```

**Benefits:**
- Reduces boilerplate from ~90 lines to ~10 lines
- Type-safe schemas extracted at compile time
- Full MCP compatibility automatically
- Smart defaults for descriptions, tags, examples

See root `CLAUDE.md` and `libs/streamlib-macros/CLAUDE.md` for details.

## Architecture Overview

```text
streamlib (this crate - public API)
├── src/lib.rs (facade - re-exports everything)
├── src/core/ (platform-agnostic traits & runtime)
├── src/apple/ (macOS/iOS implementations - INTERNAL)
├── src/linux/ (Linux implementations - INTERNAL, future)
├── src/windows/ (Windows implementations - INTERNAL, future)
├── src/python/ (PyO3 bindings)
├── src/mcp/ (MCP server for AI agents)
└── ../streamlib-macros/ (Procedural macros)
```

## The Three-Layer Design

### Layer 1: User Code (Public API)

**What users import:**

```rust
use streamlib::{
    // Processors (traits that auto-select platform impl)
    CameraProcessor, DisplayProcessor,
    AudioCaptureProcessor, AudioOutputProcessor,
    ClapEffectProcessor,

    // Data types
    VideoFrame, AudioFrame,

    // Runtime
    StreamRuntime,

    // Discovery
    ClapScanner, list_processors,
};
```

**All of these are re-exported from `src/lib.rs`.**

### Layer 2: Core (Platform-Agnostic)

**Location**: `src/core/`

**What it contains:**
- **Traits**: Define processor interfaces (`CameraProcessor`, `AudioCaptureProcessor`, etc.)
- **Types**: Data structures (`VideoFrame`, `AudioFrame`, `StreamProcessor`)
- **Runtime**: Clock, broadcaster, lifecycle management
- **Schemas**: AI/MCP discoverability
- **Ports**: Type-safe connection system

**Key files:**
- `src/core/processors/` - Processor trait definitions
- `src/core/messages.rs` - VideoFrame, AudioFrame
- `src/core/runtime.rs` - StreamRuntime
- `src/core/schema.rs` - ProcessorDescriptor, SCHEMA_VIDEO_FRAME, etc.
- `src/core/ports.rs` - StreamInput, StreamOutput

**Philosophy**: Write once, run anywhere. No `#[cfg]` in user code.

### Layer 3: Platform Implementations (Internal)

**Location**: `src/apple/`, `src/linux/`, `src/windows/`

**What they contain:**
- Concrete implementations of core traits
- Platform-specific APIs (Metal, CoreAudio, Vulkan, WASAPI, etc.)
- OS-specific optimizations

**Example**: `src/apple/processors/camera.rs`
```rust
pub struct AppleCameraProcessor {
    // Metal textures, AVFoundation capture session, etc.
}

impl CameraProcessor for AppleCameraProcessor {
    fn new(device_id: Option<usize>) -> Result<Self> {
        // Use AVFoundation to create capture session
    }
}
```

**Users never import from platform modules!**

## How Platform Selection Works

**At compile time**, the correct implementation is selected via type aliasing:

```rust
// In src/lib.rs
#[cfg(target_os = "macos")]
pub use apple::AppleCameraProcessor as CameraProcessor;

#[cfg(target_os = "linux")]
pub use linux::LinuxCameraProcessor as CameraProcessor;

#[cfg(target_os = "windows")]
pub use windows::WindowsCameraProcessor as CameraProcessor;
```

**Result**:
- User writes: `CameraProcessor::new(None)`
- On macOS: Compiles to `AppleCameraProcessor::new(None)`
- On Linux: Compiles to `LinuxCameraProcessor::new(None)`
- Zero runtime overhead, same source code!

## Module Organization

### `src/lib.rs` - Public API Facade

**Purpose**: Re-export everything users need.

```rust
// Re-export core types
pub use core::{
    StreamProcessor, StreamRuntime, VideoFrame, AudioFrame,
    ProcessorDescriptor, list_processors,
};

// Re-export platform-selected processors
#[cfg(target_os = "macos")]
pub use apple::{
    AppleCameraProcessor as CameraProcessor,
    AppleDisplayProcessor as DisplayProcessor,
    AppleAudioCaptureProcessor as AudioCaptureProcessor,
    AppleAudioOutputProcessor as AudioOutputProcessor,
};

// Re-export platform-agnostic processors (no platform variant needed)
pub use core::processors::{
    ClapEffectProcessor,  // Same on all platforms (uses clack-host)
    TestToneGenerator,    // Pure CPU, no platform deps
};
```

### `src/core/` - Platform-Agnostic Layer

**Purpose**: Define interfaces and runtime logic that work everywhere.

**Key principle**: If it has `#[cfg(target_os = ...)]`, it belongs in a platform module, not core.

**Contents**:
- `processors/` - Trait definitions
- `messages.rs` - VideoFrame, AudioFrame
- `runtime.rs` - StreamRuntime
- `schema.rs` - AI discoverability
- `ports.rs` - Type-safe connections
- `registry.rs` - Processor discovery

### `src/apple/` - macOS/iOS Implementation

**Purpose**: Platform-specific implementations using Metal, CoreAudio, AVFoundation.

**DO NOT import from here!** Use `streamlib::CameraProcessor` instead.

**Contents**:
- `processors/camera.rs` - AppleCameraProcessor
- `processors/display.rs` - AppleDisplayProcessor
- `processors/audio_capture.rs` - AppleAudioCaptureProcessor
- `processors/audio_output.rs` - AppleAudioOutputProcessor

**Technologies used**:
- Metal (GPU)
- CoreAudio (audio I/O)
- AVFoundation (camera)
- CoreVideo (CVPixelBuffer)
- IOSurface (zero-copy textures)

### `src/python/` - Python Bindings

**Purpose**: PyO3 bindings for Python decorators.

**Example**:
```python
from streamlib import camera_processor, display_processor, StreamRuntime

@camera_processor()
def camera():
    pass

@display_processor()
def display():
    pass

runtime = StreamRuntime(fps=60)
runtime.connect(camera.output_ports().video, display.input_ports().video)
runtime.run()
```

### `src/mcp/` - MCP Server

**Purpose**: Model Context Protocol server for AI agent integration.

**What it does**:
- Discovers processors via `list_processors()`
- Reads processor schemas (`ProcessorDescriptor`)
- Allows AI to build pipelines dynamically
- Manages runtime lifecycle

**Example MCP tools**:
- `add_processor` - Create processor instance
- `connect_processors` - Wire ports together
- `list_processors` - Discover available processors

## The Port Exposure Pattern

**Critical design requirement**: All processors must expose ports publicly.

### ❌ WRONG - Private Ports

```rust
pub struct AudioCaptureProcessor {
    output_port: StreamOutput<AudioFrame>,  // Private!
}

// User can't connect this - no access to port!
```

### ✅ CORRECT - Public Ports

```rust
pub struct AudioCaptureOutputPorts {
    pub audio: StreamOutput<AudioFrame>,
}

pub struct AudioCapturePorts {
    pub output: AudioCaptureOutputPorts,
}

pub struct AudioCaptureProcessor {
    pub ports: AudioCapturePorts,  // Public!
    // ... private fields
}

// User can connect:
runtime.connect(
    &mut mic.ports.output.audio,
    &mut speaker.ports.input.audio
)?;
```

**Why this matters**:
1. **Type safety**: Compiler verifies port types match
2. **Discoverability**: AI can introspect ports
3. **Consistency**: Same pattern everywhere (video, audio, effects)

## The CLAP Philosophy

**CLAP is a required dependency, not optional.**

Just as `wgpu` is the standard GPU layer, **CLAP is the standard audio processing layer**.

```rust
use streamlib::{ClapEffectProcessor, ClapScanner};

// Discover installed plugins
let plugins = ClapScanner::scan_system_plugins()?;

// Load reverb plugin
let reverb = ClapEffectProcessor::load(&plugins[0].path)?;
reverb.activate(48000, 2048)?;

// Connect in pipeline
runtime.connect(&mut mic.ports.output.audio, &mut reverb.ports.input.audio)?;
runtime.connect(&mut reverb.ports.output.audio, &mut speaker.ports.input.audio)?;
```

**CLAP = Audio Shaders**. Just as WGSL shaders process video on GPU, CLAP plugins process audio.

## Import Guidelines

### ✅ DO Import From `streamlib`

```rust
use streamlib::{
    CameraProcessor, DisplayProcessor,
    AudioCaptureProcessor, AudioOutputProcessor,
    ClapEffectProcessor, ClapScanner,
    VideoFrame, AudioFrame,
    StreamRuntime, StreamProcessor,
    ProcessorDescriptor, list_processors,
};
```

### ❌ DON'T Import From Platform Modules

```rust
// ❌ WRONG - Breaks on other platforms!
use streamlib::apple::AppleCameraProcessor;
use streamlib::apple::AppleDisplayProcessor;

// ❌ WRONG - Internal implementation detail!
use streamlib::core::processors::camera::CameraProcessor;
```

### ❌ DON'T Import From `streamlib_core` Directly

```rust
// ❌ WRONG - Use `streamlib` instead
use streamlib_core::StreamRuntime;

// ✅ CORRECT
use streamlib::StreamRuntime;
```

**Why?** The top-level `streamlib` crate is the public API. Everything else is internal.

## Connection Patterns

### Type-Safe Connections (Rust)

```rust
let mut camera = CameraProcessor::new(None)?;
let mut display = DisplayProcessor::new(None, 1920, 1080)?;

// Type-safe: Compiler verifies VideoFrame → VideoFrame
runtime.connect(
    &mut camera.ports.output.video,
    &mut display.ports.input.video
)?;
```

### String-Based Connections (MCP Only)

```rust
// Only for dynamic runtime connections via MCP
runtime.connect_at_runtime("processor_0.video", "processor_1.video").await?;
```

**Do NOT use string connections in regular Rust code!** They bypass type safety.

## Testing & Examples

### Unit Tests
- Located in each module (e.g., `src/core/processors/camera.rs`)
- Test traits and core logic
- Platform-agnostic

### Integration Tests
- Located in `tests/`
- Test full pipelines
- Platform-specific (use `#[cfg(target_os = ...)]`)

### Examples
- Located in `/examples/` (NOT `libs/streamlib/examples/`)
- Each example is a standalone project with own `Cargo.toml`
- Demonstrates real-world usage patterns

**Example structure**:
```
examples/microphone-reverb-speaker/
├── Cargo.toml (depends on `streamlib = { path = "../../libs/streamlib" }`)
├── project.json (Nx configuration)
├── README.md
└── src/main.rs
```

## When To Look Where

| Task | Location |
|------|----------|
| **Using streamlib** | Import from `streamlib::*` |
| **Understanding processor interfaces** | Read `src/core/processors/` |
| **Debugging platform issues** | Check `src/apple/` (or linux/windows) |
| **Adding new processor type** | Define trait in `src/core/processors/`, implement in `src/apple/` |
| **MCP integration** | `src/mcp/` |
| **Python bindings** | `src/python/` |

## Related Documentation

- `/CLAUDE.md` - Repository-wide guidelines
- `src/core/CLAUDE.md` - Core layer details
- `src/apple/CLAUDE.md` - Platform implementation notes
- `examples/*/README.md` - Usage examples
