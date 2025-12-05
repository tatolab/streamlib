// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

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

use streamlib::core::{CameraConfig, DisplayConfig};
use streamlib::{input, output, CameraProcessor, DisplayProcessor, Result, StreamRuntime};

mod lower_third;
use lower_third::LowerThirdProcessor;

fn main() -> Result<()> {
    // Initialize logging
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    tracing::info!("=== News-Cast Example ===");
    tracing::info!("Pipeline: Camera → Lower Third → Display");

    // Create runtime (event-driven, no FPS parameter!)
    let mut runtime = StreamRuntime::new();

    // 1. Add camera processor
    tracing::info!("Adding camera processor...");
    let camera = runtime.add_processor::<CameraProcessor::Processor>(CameraConfig {
        device_id: Some("0x1424001bcf2284".to_string()),
    })?;

    // 2. Add lower third effect processor
    tracing::info!("Adding lower third effect processor...");
    let lower_third =
        runtime.add_processor::<LowerThirdProcessor::Processor>(lower_third::LowerThirdConfig {
            headline: "BREAKING NEWS".to_string(),
            subtitle: "StreamLib Rust Migration Complete".to_string(),
        })?;

    // 3. Add display processor
    tracing::info!("Adding display processor...");
    let display = runtime.add_processor::<DisplayProcessor::Processor>(DisplayConfig {
        width: 1920,
        height: 1080,
        title: Some("News Cast - StreamLib Demo".to_string()),
        scaling_mode: Default::default(), // Use default scaling (Stretch)
    })?;

    // Connect pipeline using type-safe port markers: Camera → Lower Third → Display
    tracing::info!("Connecting pipeline...");
    runtime.connect(
        output::<CameraProcessor::OutputLink::video>(&camera),
        input::<LowerThirdProcessor::InputLink::input>(&lower_third),
    )?;
    runtime.connect(
        output::<LowerThirdProcessor::OutputLink::output>(&lower_third),
        input::<DisplayProcessor::InputLink::video>(&display),
    )?;

    // Start the runtime
    tracing::info!("Starting runtime...");
    tracing::info!("You should see:");
    tracing::info!("  - Live camera feed in window");
    tracing::info!("  - Blue bar sliding up from bottom");
    tracing::info!("  - Gold accent line");

    // start() blocks on macOS standalone (runs NSApplication event loop)
    runtime.start()?;

    tracing::info!("Runtime stopped");

    Ok(())
}
