// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Camera → Cyberpunk Compositor → Display Pipeline Example
//!
//! Demonstrates a full video processing pipeline with:
//! - Rust Vision-based person segmentation with cyberpunk background compositing
//! - Python lower third overlay with slide-in animation
//! - Python glitch effect (RGB separation, scanlines, slice displacement)
//! - Zero-copy GPU texture sharing via IOSurface
//!
//! Pipeline: Camera → CyberpunkCompositor (Rust) → CyberpunkLowerThird → CyberpunkGlitch → Display
//!
//! ## Prerequisites
//!
//! - `uv` must be installed: https://docs.astral.sh/uv/
//!
//! ## Usage
//!
//! ```bash
//! cargo run -p camera-python-display
//! ```

mod cyberpunk_compositor;

use std::path::PathBuf;
use streamlib::core::{InputLinkPortRef, OutputLinkPortRef};
use streamlib::{
    ApiServerConfig, ApiServerProcessor, CameraProcessor, DisplayProcessor, Result, StreamRuntime,
};
use streamlib_python::{PythonHostProcessor, PythonHostProcessorConfig};

use cyberpunk_compositor::{CyberpunkCompositorConfig, CyberpunkCompositorProcessor};

fn main() -> Result<()> {
    // Initialize tracing subscriber FIRST
    let subscriber = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
                "info,naga=warn,wgpu_core=warn,wgpu_hal=warn"
                    .parse()
                    .unwrap()
            }),
        )
        .finish();

    tracing::subscriber::set_global_default(subscriber).expect("Failed to set tracing subscriber");

    // THEN initialize LogTracer to forward Python logging (via pyo3-log) to tracing
    tracing_log::LogTracer::init().expect("Failed to initialize LogTracer");

    println!("=== Camera → Python Cyberpunk → Display Pipeline ===\n");

    let runtime = StreamRuntime::new()?;

    // =========================================================================
    // Add Camera processor
    // =========================================================================

    println!("📷 Adding camera processor...");
    let camera = runtime.add_processor(CameraProcessor::node(CameraProcessor::Config {
        device_id: None,
    }))?;
    println!("✓ Camera added: {}\n", camera);

    // =========================================================================
    // Add Rust Cyberpunk Compositor (Vision segmentation + background)
    // =========================================================================

    println!(
        "🦀 Adding Rust cyberpunk compositor (Vision segmentation + background replacement)..."
    );

    let project_path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("python");
    let background_image = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/background.png");

    let cyberpunk = runtime.add_processor(CyberpunkCompositorProcessor::node(
        CyberpunkCompositorConfig {
            background_image_path: if background_image.exists() {
                Some(background_image)
            } else {
                tracing::warn!("Background image not found at assets/background.png, using procedural background");
                None
            },
            ..Default::default()
        },
    ))?;
    println!("✓ Rust cyberpunk compositor added: {}\n", cyberpunk);

    // =========================================================================
    // Add Python Cyberpunk Lower Third processor
    // =========================================================================

    println!("🐍 Adding Python cyberpunk lower third processor (slide-in overlay)...");

    let lower_third =
        runtime.add_processor(PythonHostProcessor::node(PythonHostProcessorConfig {
            project_path: project_path.clone(),
            class_name: "CyberpunkLowerThird".to_string(),
            entry_point: Some("cyberpunk_lower_third.py".to_string()),
        }))?;
    println!(
        "✓ Python cyberpunk lower third processor added: {}\n",
        lower_third
    );

    // =========================================================================
    // Add Python Cyberpunk Watermark processor
    // =========================================================================

    println!("🐍 Adding Python cyberpunk watermark processor...");

    let watermark =
        runtime.add_processor(PythonHostProcessor::node(PythonHostProcessorConfig {
            project_path: project_path.clone(),
            class_name: "CyberpunkWatermark".to_string(),
            entry_point: Some("cyberpunk_watermark.py".to_string()),
        }))?;
    println!(
        "✓ Python cyberpunk watermark processor added: {}\n",
        watermark
    );

    // =========================================================================
    // Add Python Cyberpunk Glitch processor (post-processing) - TEMPORARILY DISABLED
    // =========================================================================

    // println!("🐍 Adding Python cyberpunk glitch processor (RGB separation, scanlines)...");
    //
    // let glitch = runtime.add_processor(PythonHostProcessor::node(PythonHostProcessorConfig {
    //     project_path,
    //     class_name: "CyberpunkGlitch".to_string(),
    //     entry_point: Some("cyberpunk_glitch.py".to_string()),
    // }))?;
    // println!("✓ Python cyberpunk glitch processor added: {}\n", glitch);
    let _ = project_path; // suppress unused warning

    // =========================================================================
    // Add Display processor
    // =========================================================================

    println!("🖥️  Adding display processor...");
    let display = runtime.add_processor(DisplayProcessor::node(DisplayProcessor::Config {
        width: 1920,
        height: 1080,
        title: Some("Camera → Compositor → Lower Third → Watermark → Display".to_string()),
        scaling_mode: Default::default(),
    }))?;
    println!("✓ Display added: {}\n", display);

    // =========================================================================
    // Add API Server processor (free-floating, for registry inspection)
    // =========================================================================

    println!("🌐 Adding API server processor...");
    let _api_server = runtime.add_processor(ApiServerProcessor::node(ApiServerConfig {
        host: "127.0.0.1".to_string(),
        port: 9000,
    }))?;
    println!("✓ API server running at http://127.0.0.1:9000");
    println!("   Registry: http://127.0.0.1:9000/registry\n");

    // =========================================================================
    // Connect the pipeline: Camera → Compositor → Lower Third → Watermark → Display
    // (Glitch processor temporarily disabled)
    // =========================================================================

    println!("🔗 Connecting pipeline...");

    // Camera video → Compositor video_in
    runtime.connect(
        OutputLinkPortRef::new(&camera, "video"),
        InputLinkPortRef::new(&cyberpunk, "video_in"),
    )?;
    println!("   ✓ Camera → Compositor");

    // Compositor video_out → Lower Third video_in
    runtime.connect(
        OutputLinkPortRef::new(&cyberpunk, "video_out"),
        InputLinkPortRef::new(&lower_third, "video_in"),
    )?;
    println!("   ✓ Compositor → Lower Third");

    // Lower Third video_out → Watermark video_in
    runtime.connect(
        OutputLinkPortRef::new(&lower_third, "video_out"),
        InputLinkPortRef::new(&watermark, "video_in"),
    )?;
    println!("   ✓ Lower Third → Watermark");

    // Watermark video_out → Display video
    runtime.connect(
        OutputLinkPortRef::new(&watermark, "video_out"),
        InputLinkPortRef::new(&display, "video"),
    )?;
    println!("   ✓ Watermark → Display");
    println!();

    // =========================================================================
    // Run the pipeline
    // =========================================================================

    println!("▶️  Starting pipeline...");
    #[cfg(target_os = "macos")]
    println!("   Press Cmd+Q to stop\n");
    #[cfg(not(target_os = "macos"))]
    println!("   Press Ctrl+C to stop\n");

    runtime.start()?;
    runtime.wait_for_signal()?;

    println!("\n✓ Pipeline stopped gracefully");

    Ok(())
}
