# Simple Pipeline

The simplest possible streamlib pipeline: a test tone generator connected to audio output.

## What This Example Demonstrates

- **Event-driven processing** - No FPS/tick parameters needed
- **Config-based processor creation** - Using `TestToneConfig` and `AudioOutputConfig`
- **Handle-based type-safe connections** - Compiler verifies `AudioFrame → AudioFrame` matches
- **Runtime management** - Start, run, and stop the pipeline

## What You'll Experience

When you run this example, you'll hear a 440 Hz tone (musical note A4) for 2 seconds.

## Running

```bash
cargo run -p simple-pipeline
```

## Code Walkthrough

1. **Create runtime** - `StreamRuntime::new()` (no FPS parameter!)
2. **Add processors** - `runtime.add_processor_with_config::<T>(config)`
3. **Connect processors** - `runtime.connect(output_port, input_port)`
4. **Start pipeline** - `runtime.start().await`
5. **Stop pipeline** - `runtime.stop().await`

## Key Concepts

- **Global audio config** - All audio processors share the runtime's sample rate and channel configuration
- **Type-safe connections** - The compiler prevents connecting mismatched types
- **Platform-agnostic** - Same code works on macOS, Linux, Windows
