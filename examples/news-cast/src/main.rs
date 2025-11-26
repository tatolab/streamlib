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

use streamlib::{Result, StreamRuntime, CameraProcessor, DisplayProcessor, VideoFrame};
use streamlib::core::{CameraConfig, DisplayConfig, PerformanceOverlayConfig};

// Import core processors (performance overlay requires debug-overlay feature)
use streamlib::core::PerformanceOverlayProcessor;

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

    // Create runtime (event-driven, no FPS parameter!)
    let mut runtime = StreamRuntime::new();

    // 1. Add camera processor using config-based API
    tracing::info!("Adding camera processor...");
    let camera = runtime.add_processor::<CameraProcessor>(
        CameraConfig {
            device_id: Some("0x1424001bcf2284".to_string()),
        }
    )?;

    // 2. Add lower third effect processor (custom processor - needs config struct)
    tracing::info!("Adding lower third effect processor...");
    let lower_third = runtime.add_processor::<LowerThirdProcessor>(
        lower_third::LowerThirdConfig {
            headline: "BREAKING NEWS".to_string(),
            subtitle: "StreamLib Rust Migration Complete".to_string(),
        }
    )?;

    // 3. Add performance overlay processor
    tracing::info!("Adding performance overlay processor...");
    let perf_overlay = runtime.add_processor::<PerformanceOverlayProcessor>(
        PerformanceOverlayConfig {}
    )?;

    // 4. Add display processor
    tracing::info!("Adding display processor...");
    let display = runtime.add_processor::<DisplayProcessor>(
        DisplayConfig {
            width: 1920,
            height: 1080,
            title: Some("News Cast - StreamLib Demo".to_string()),
            scaling_mode: Default::default(),  // Use default scaling (Stretch)
        }
    )?;

    // Connect pipeline using type-safe handles: Camera → Lower Third → Performance Overlay → Display
    tracing::info!("Connecting pipeline...");
    runtime.connect(
        camera.output_port::<VideoFrame>("video"),
        lower_third.input_port::<VideoFrame>("input"),
    )?;
    runtime.connect(
        lower_third.output_port::<VideoFrame>("output"),
        perf_overlay.input_port::<VideoFrame>("video"),
    )?;
    runtime.connect(
        perf_overlay.output_port::<VideoFrame>("video"),
        display.input_port::<VideoFrame>("video"),
    )?;

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
