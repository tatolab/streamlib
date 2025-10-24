//! News-Cast Example
//!
//! Demonstrates GPU-native video effects with a news-style lower third overlay.
//!
//! Pipeline: Camera → Lower Third Effect → Performance Overlay → Display
//!
//! This example shows:
//! - Zero-copy camera capture (IOSurface)
//! - GPU-native effects (vello compute shaders)
//! - Real-time compositing (WebGPU)
//! - Performance monitoring (FPS overlay)
//! - Display output (Metal blit encoder)
//!
//! Everything stays on GPU - no CPU copies!

use streamlib::{Result, StreamRuntime};

// Import platform-specific processor implementations
use streamlib::{CameraProcessor, DisplayProcessor};

// Import core processors (performance overlay requires debug-overlay feature)
use streamlib::core::PerformanceOverlayProcessor;

// Import traits for their methods
use streamlib::core::processors::{
    CameraProcessor as CameraProcessorTrait,
    DisplayProcessor as DisplayProcessorTrait,
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
    tracing::info!("Pipeline: Camera → Lower Third → Performance Overlay → Display");

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

    // 3. Performance overlay processor
    tracing::info!("Creating performance overlay processor...");
    let mut perf_overlay = PerformanceOverlayProcessor::new()?;

    // 4. Display processor
    tracing::info!("Creating display processor...");
    let mut display = DisplayProcessor::with_size(1920, 1080)?;
    display.set_window_title("News Cast - StreamLib Demo");

    // Connect pipeline: Camera → Lower Third → Performance Overlay → Display
    tracing::info!("Connecting pipeline...");
    runtime.connect(
        &mut camera.output_ports().video,
        &mut lower_third.input_ports().video,
    )?;
    runtime.connect(
        &mut lower_third.output_ports().video,
        &mut perf_overlay.input_ports().video,
    )?;
    runtime.connect(
        &mut perf_overlay.output_ports().video,
        &mut display.input_ports().video,
    )?;

    // Add processors to runtime
    runtime.add_processor(Box::new(camera));
    runtime.add_processor(Box::new(lower_third));
    runtime.add_processor(Box::new(perf_overlay));
    runtime.add_processor(Box::new(display));

    // Start the runtime
    tracing::info!("Starting runtime...");
    runtime.start().await?;

    tracing::info!("Runtime started! Press Ctrl+C to stop.");
    tracing::info!("You should see:");
    tracing::info!("  - Live camera feed in window");
    tracing::info!("  - Blue bar sliding up from bottom");
    tracing::info!("  - Gold accent line");
    tracing::info!("  - Performance overlay with FPS graph (top-left corner)");

    // Run forever (until Ctrl+C)
    runtime.run().await?;

    Ok(())
}
