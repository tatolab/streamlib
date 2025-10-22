//! Camera to Display Pipeline Example
//!
//! Demonstrates a real-time camera → display pipeline using streamlib's
//! processor-based API. The CameraProcessor and DisplayProcessor handle all
//! platform-specific details internally.
//!
//! The same code works on macOS, Linux, and Windows - streamlib automatically
//! configures itself for the platform at runtime.
//!
//! Press Ctrl+C to stop.

use streamlib::{
    CameraProcessor, CameraProcessorTrait, DisplayProcessor, DisplayProcessorTrait, Result,
    StreamRuntime,
};

#[tokio::main]
async fn main() -> Result<()> {
    println!("=== Camera → Display Pipeline ===\n");

    // Create runtime - automatically configured for the platform
    let mut runtime = StreamRuntime::new(60.0);

    // Create camera and display processors
    // These handle all platform-specific details (AVFoundation, NSWindow, etc.)
    // Using default camera to avoid Continuity Camera issues
    let mut camera = CameraProcessor::with_device_id("0x1424001bcf2284")?;
    let mut display = DisplayProcessor::new()?;

    // Set display window title
    display.set_window_title("streamlib Camera Display");

    // Connect camera output → display input
    println!("🔗 Connecting camera → display...");
    runtime.connect(
        &mut camera.output_ports().video,
        &mut display.input_ports().video,
    )?;
    println!("✓ Pipeline connected\n");

    // Add processors to runtime
    runtime.add_processor(Box::new(camera));
    runtime.add_processor(Box::new(display));

    // Start pipeline
    println!("▶️  Starting pipeline (60 fps)...");
    println!("   Press Ctrl+C to stop\n");
    runtime.start().await?;

    // Run until Ctrl+C
    runtime.run().await?;

    println!("\n✓ Pipeline stopped gracefully");

    Ok(())
}
