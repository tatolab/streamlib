# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What is Streamlib?

Streamlib is a real-time GPU-accelerated streaming infrastructure for AI agents, built in Rust with Python bindings. It enables sub-millisecond latency video/audio processing with zero CPU copies, targeting robotics, AR/VR, and edge AI systems.

**Core vision:** AI agents exchange video effects as HLSL shader source code, compiled locally to Metal/Vulkan, executed on GPU with zero-copy pipelines.

## Common Commands

### Build
```bash
# Build entire workspace
cargo build

# Build with features
cargo build --features debug-overlay
cargo build --features mcp
cargo build --features python

# Build MCP server binary
cargo build --bin streamlib-mcp --features mcp
```

### Testing
```bash
# Run all tests
cargo test

# Run tests for specific package
cargo test -p streamlib
cargo test -p streamlib-macros

# Run specific test
cargo test test_name
```

### Running Examples
```bash
# Rust examples
cargo run --example simple-pipeline
cargo run --example camera-display
cargo run --example audio-mixer-demo
cargo run --example microphone-reverb-speaker

# Python examples (requires building Python bindings first)
cd examples/simple-camera-display
python main.py
```

### Python Development
```bash
# Build Python wheel
cd libs/streamlib
maturin develop --features python

# Build release wheel
maturin build --release --features python
```

### Documentation
```bash
# Generate and open documentation
cargo doc --open --all-features
```

### Debugging
```bash
# Run with trace logging
RUST_LOG=trace cargo run --example camera-display

# Debug specific module
RUST_LOG=streamlib=trace cargo run
RUST_LOG=info,streamlib::core::runtime=trace cargo run
```

## Architecture Overview

### Core Abstractions

**Processor Graph Architecture:**
- **StreamProcessor** - Base trait for all processors (sources, sinks, transformers)
- **Sources** - Generate data (CameraProcessor, AudioCaptureProcessor, test generators)
- **Sinks** - Consume data (DisplayProcessor, AudioOutputProcessor)
- **Transformers** - Process data (ClapEffectProcessor, AudioMixerProcessor)

**Type-Safe Port Connections:**
- `StreamOutput<T>` and `StreamInput<T>` - Generic ports where T implements `PortMessage`
- `VideoFrame` - GPU texture wrapper with metadata
- `AudioFrame<CHANNELS>` - Audio samples (const generic for channel count: 1, 2, 4, 6, 8)
- `DataFrame` - Generic metadata/control messages
- Connections use `ProcessorConnection<T>` with lock-free ring buffers (rtrb)

**Event-Driven Processing:**
- No explicit tick loops - processors wake on events:
  - `WakeupEvent::DataAvailable` - Downstream has data ready
  - `WakeupEvent::TimerTick` - Clock-based wakeup
  - `WakeupEvent::Shutdown` - Graceful shutdown
- Each processor runs in its own thread with a wakeup channel

### Key Files

**Core runtime (libs/streamlib/src/core/):**
- `runtime.rs` - Main StreamRuntime engine (1278 lines, orchestrates everything)
- `bus.rs` - Connection management (wraps ConnectionManager)
- `connection.rs` - ProcessorConnection<T> with lock-free ring buffer
- `handles.rs` - ProcessorHandle, OutputPortRef<T>, InputPortRef<T>
- `ports.rs` - StreamOutput<T>, StreamInput<T> implementations

**Frames (libs/streamlib/src/core/frames/):**
- `video_frame.rs` - VideoFrame wraps wgpu::Texture (GPU texture)
- `audio_frame.rs` - AudioFrame<CHANNELS> with dasp integration
- `data_frame.rs` - DataFrame (HashMap-based metadata)

**Processors (libs/streamlib/src/core/):**
- `sources/` - Camera, audio capture, test generators
- `sinks/` - Display, audio output
- `transformers/` - CLAP effects, audio mixer, performance overlay

**Platform-specific (libs/streamlib/src/apple/):**
- `sources/camera.rs` - AppleCameraProcessor (AVFoundation)
- `sinks/display.rs` - AppleDisplayProcessor (Metal → NSWindow)
- `wgpu_bridge.rs` - CVPixelBuffer ↔ wgpu::Texture conversion (zero-copy via IOSurface)

### Critical Design Patterns

**Handle-Based Type-Safe API:**
```rust
let camera: ProcessorHandle = runtime.add_processor::<CameraProcessor>()?;
let output: OutputPortRef<VideoFrame> = camera.output_port("video");
let input: InputPortRef<VideoFrame> = display.input_port("video");
runtime.connect(output, input)?;  // Compile-time type safety!
```

**Zero-Copy GPU Pipeline (macOS):**
```
AVCaptureSession → CVPixelBuffer → IOSurface → Metal Texture → wgpu::Texture → Display
                                     ↑
                              (Zero CPU copy - GPU memory shared!)
```

**Inventory-Based Auto-Registration:**
```rust
// Processors register globally at compile-time
inventory::submit! {
    &CameraProcessorDescriptor as &dyn DescriptorProvider
}

// Runtime discovers all processors
global_registry().lock().list()
```

**Const Generic Audio Channels:**
```rust
AudioFrame<2>   // Stereo - compile-time verified
AudioFrame<6>   // 5.1 surround
AudioFrame<8>   // 7.1 surround
```

## Feature Flags

- `default` - Core functionality only
- `mcp` - Enable MCP server for AI agent integration (requires `rmcp`, `axum`)
- `python` - Python bindings via PyO3 (extension module for .so/.dylib)
- `python-embed` - Embed Python in binaries (for MCP server running Python processors)
- `debug-overlay` - GPU performance overlay with vello rendering

## Platform Abstraction

**macOS/iOS specific code:**
- Located in `libs/streamlib/src/apple/`
- Uses objc2 ecosystem for Objective-C bindings
- Metal for GPU backend
- AVFoundation for camera/audio
- IOSurface for zero-copy texture sharing

**Platform-agnostic code:**
- Located in `libs/streamlib/src/core/`
- Uses wgpu for cross-platform GPU abstraction

**Conditional compilation pattern:**
```rust
#[cfg(any(target_os = "macos", target_os = "ios"))]
pub use apple::AppleCameraProcessor as CameraProcessor;

// User code is platform-agnostic
use streamlib::CameraProcessor;
```

## Workspace Structure

This is a hybrid Cargo workspace + Nx monorepo:
- **Cargo** manages Rust compilation
- **Nx** orchestrates monorepo tasks (via `@monodon/rust` plugin)
- **Maturin** builds Python wheels (pyproject.toml in libs/streamlib)

**Workspace members:**
```
libs/streamlib           # All-in-one crate (core + apple + mcp + python)
libs/streamlib-macros    # Procedural macros
examples/*               # Example applications
```

## Adding New Processors

1. Create processor struct implementing `StreamProcessor` trait
2. Implement required methods:
   - `from_config()` - Factory method
   - `setup()` - Initialize resources
   - `process()` - Main processing loop
   - `teardown()` - Cleanup
3. Add descriptor for MCP server discovery:
```rust
inventory::submit! {
    &YourProcessorDescriptor as &dyn DescriptorProvider
}
```
4. Register platform-specific implementations in lib.rs if needed

## Port Connection Rules

- Output ports are created with `output_port.add_port::<T>("name")`
- Input ports are created with `input_port.add_port::<T>("name")`
- Connections require matching types: `ProcessorConnection<T>` validates at compile time
- Use `Bus` to manage connections: `bus.create_connection::<T>(capacity)`
- Processors wake up automatically when data is available (event-driven)

## Audio Processing Notes

- All audio uses lock-free ring buffers (rtrb) - real-time safe
- Sample rate conversion via rubato (real-time safe)
- CLAP plugin hosting for audio effects (clack-host)
- No allocations in audio callback paths
- AudioContext provides global sample rate and buffer size

## GPU Context

- WgpuContext provides wgpu device/queue/adapter
- All processors share the same GPU context
- VideoFrame wraps Arc<wgpu::Texture> for zero-copy sharing
- On macOS, Metal textures can be created from IOSurface for zero-copy interop

## MCP Server (AI Agent Integration)

**Binary:** `cargo build --bin streamlib-mcp --features mcp`

**Capabilities:**
- List available processors (via global registry)
- Add/remove processors at runtime
- Connect/disconnect ports
- Query processor schemas (inputs, outputs, config)
- Execute Python processor code (when python-embed feature enabled)

**Protocol:** MCP (Model Context Protocol) over stdio or HTTP

## Python Bindings

**Build:** `maturin develop --features python`

**API mirrors Rust:**
```python
runtime = streamlib.StreamRuntime()
camera = runtime.add_processor(streamlib.CameraProcessor())
runtime.connect(camera.output_port("video"), display.input_port("video"))
await runtime.run()
```

## Important Conventions

- **No explicit tick loops** - use event-driven wakeups
- **Lock-free where possible** - especially for audio/video data paths
- **Zero-copy GPU pipelines** - use Arc<wgpu::Texture> and platform interop (IOSurface)
- **Type safety at compile time** - generic ports prevent connection mismatches
- **Platform abstraction via traits** - core defines traits, platform modules implement
- **Inventory-based registration** - processors self-register at compile time

## Debugging Tips

- Use `RUST_LOG=trace` to see all log output
- Debug overlay shows FPS, GPU memory, frame timing (requires debug-overlay feature)
- Check processor wakeup events to understand scheduling
- Use `cargo tree` to debug dependency issues
- On macOS, check permissions for camera/screen recording in System Settings
