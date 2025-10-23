//! News-Cast Example
//!
//! Demonstrates GPU-native video effects with a news-style lower third overlay.
//!
//! Pipeline: Camera → Lower Third Effect → Display
//!
//! This example shows:
//! - Zero-copy camera capture (IOSurface)
//! - GPU-native effects (vello compute shaders)
//! - Real-time compositing (WebGPU)
//! - Display output (Metal blit encoder)
//!
//! Everything stays on GPU - no CPU copies!

use streamlib::{
    Result, StreamRuntime,
    CameraProcessor, CameraProcessorTrait,
    DisplayProcessor, DisplayProcessorTrait,
};

mod lower_third;
use lower_third::LowerThirdProcessor;

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize logging
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    tracing::info!("=== News-Cast Example ===");
    tracing::info!("Pipeline: Camera → Lower Third → Display");

    // Create runtime at 60fps
    let mut runtime = StreamRuntime::new(60.0);

    // 1. Camera processor (captures from default camera)
    tracing::info!("Creating camera processor...");
    let mut camera = CameraProcessor::with_device_id("0x1424001bcf2284")?;

    // 2. Lower third effect processor
    tracing::info!("Creating lower third effect processor...");
    let mut lower_third = LowerThirdProcessor::new(
        "BREAKING NEWS".to_string(),
        "StreamLib Rust Migration Complete".to_string(),
    )?;

    // 3. Display processor
    tracing::info!("Creating display processor...");
    let mut display = DisplayProcessor::with_size(1920, 1080)?;
    display.set_window_title("News Cast - StreamLib Demo");

    // Connect pipeline: Camera → Lower Third → Display
    tracing::info!("Connecting pipeline...");
    runtime.connect(
        &mut camera.output_ports().video,
        &mut lower_third.input_ports().video,
    )?;
    runtime.connect(
        &mut lower_third.output_ports().video,
        &mut display.input_ports().video,
    )?;

    // Add processors to runtime
    runtime.add_processor(Box::new(camera));
    runtime.add_processor(Box::new(lower_third));
    runtime.add_processor(Box::new(display));

    // Start the runtime
    tracing::info!("Starting runtime...");
    runtime.start().await?;

    tracing::info!("Runtime started! Press Ctrl+C to stop.");
    tracing::info!("You should see:");
    tracing::info!("  - Live camera feed in window");
    tracing::info!("  - Blue bar sliding up from bottom");
    tracing::info!("  - Gold accent line");
    tracing::info!("  - (Text rendering coming in future iteration)");

    // Run forever (until Ctrl+C)
    runtime.run().await?;

    Ok(())
}
