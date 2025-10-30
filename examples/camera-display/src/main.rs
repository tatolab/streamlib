//! Camera to Display Pipeline Example
//!
//! Demonstrates a real-time camera → display pipeline using streamlib's
//! processor-based API with **type-safe connections**.
//!
//! ## Key Features
//!
//! - **Type-safe connections**: Compiler enforces VideoFrame → VideoFrame matching
//! - **Event-driven**: Camera wakes display on each frame (push-based, not ticks)
//! - **Platform-agnostic**: Same code works on macOS, Linux, Windows
//! - **Zero-copy GPU**: Frames stay on GPU from camera to display
//!
//! ## How It Works
//!
//! 1. Camera captures frames and writes to output port
//! 2. Display wakes up immediately on data arrival (no polling)
//! 3. Display reads frame and renders to window
//!
//! Press Ctrl+C to stop.

use streamlib::{Result, StreamRuntime};

// Import platform-specific processor implementations
// On macOS: AppleCameraProcessor and AppleDisplayProcessor
// These are re-exported as CameraProcessor and DisplayProcessor
use streamlib::{CameraProcessor, DisplayProcessor};

// Import traits for their methods (output_ports, input_ports, etc.)
use streamlib::core::processors::{
    CameraProcessor as CameraProcessorTrait,
    DisplayProcessor as DisplayProcessorTrait,
};

#[tokio::main]
async fn main() -> Result<()> {
    println!("=== Camera → Display Pipeline ===\n");

    // Create runtime - automatically configured for the platform
    // Event-driven: processors wake on data arrival or timer events
    let mut runtime = StreamRuntime::new();

    // Create camera and display processors
    // These handle all platform-specific details (AVFoundation, NSWindow, etc.)
    // Using default camera to avoid Continuity Camera issues
    let mut camera = CameraProcessor::with_device_id("0x11424001bcf2284")?;
    let mut display = DisplayProcessor::new()?;

    // Set display window title
    display.set_window_title("streamlib Camera Display");

    // Connect camera output → display input (TYPE-SAFE!)
    // The compiler ensures both ports use VideoFrame - mismatched types won't compile
    println!("🔗 Connecting camera → display (type-safe)...");
    runtime.connect(
        &mut camera.output_ports().video,   // StreamOutput<VideoFrame>
        &mut display.input_ports().video,   // StreamInput<VideoFrame>
    )?;
    println!("✓ Pipeline connected\n");

    // Add processors to runtime
    runtime.add_processor(Box::new(camera));
    runtime.add_processor(Box::new(display));

    // Start pipeline
    println!("▶️  Starting pipeline...");
    println!("   Press Ctrl+C to stop\n");
    runtime.start().await?;

    // Run until Ctrl+C
    runtime.run().await?;

    println!("\n✓ Pipeline stopped gracefully");

    Ok(())
}
