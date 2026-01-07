// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Camera → Cyberpunk Pipeline (Parallel Blending)
//!
//! Parallel video processing pipeline with multi-layer compositing:
//! - Rust Vision-based person segmentation with cyberpunk color grading
//! - Python lower third overlay (continuous RGBA generator)
//! - Python watermark overlay (continuous RGBA generator)
//! - Rust blending compositor (alpha blends all layers)
//! - Python glitch effect (RGB separation, scanlines, slice displacement)
//!
//! Pipeline Architecture:
//! ```
//!   Camera ──→ Cyberpunk ──→ BlendingCompositor ──→ Glitch ──→ Display
//!                                  ↑         ↑
//!   LowerThird (16ms) ─────────────┘         │
//!   Watermark (16ms) ────────────────────────┘
//! ```
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

mod blending_compositor;
mod cyberpunk_compositor;

use std::path::PathBuf;
use streamlib::core::{InputLinkPortRef, OutputLinkPortRef};
use streamlib::{
    ApiServerConfig, ApiServerProcessor, CameraProcessor, DisplayProcessor, Result, StreamRuntime,
};
use streamlib_python::{PythonContinuousHostProcessor, PythonProcessorConfig};

use blending_compositor::{BlendingCompositorConfig, BlendingCompositorProcessor};
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

    println!("=== Camera → Cyberpunk Pipeline (Parallel Blending) ===\n");

    let runtime = StreamRuntime::new()?;
    let project_path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("python");

    // =========================================================================
    // Camera Source
    // =========================================================================

    println!("📷 Adding camera processor...");
    let camera = runtime.add_processor(CameraProcessor::node(CameraProcessor::Config {
        device_id: None,
        ..Default::default()
    }))?;
    println!("✓ Camera added: {}\n", camera);

    // Vision-based person segmentation with cinematic color grading
    println!("🦀 Adding Rust cyberpunk compositor (Vision segmentation)...");
    let cyberpunk = runtime.add_processor(CyberpunkCompositorProcessor::node(
        CyberpunkCompositorConfig::default(),
    ))?;
    println!("✓ Rust cyberpunk compositor added: {}\n", cyberpunk);

    // =========================================================================
    // Blending Compositor (Parallel layer blending)
    // =========================================================================

    println!("🎨 Adding blending compositor (parallel layer blending)...");
    let blending = runtime.add_processor(BlendingCompositorProcessor::node(
        BlendingCompositorConfig::default(),
    ))?;
    println!("✓ Blending compositor added: {}\n", blending);

    // =========================================================================
    // PARALLEL: Lower Third (continuous RGBA generator)
    // =========================================================================

    println!("🐍 Adding Python lower third GENERATOR (parallel, 16ms)...");
    let lower_third =
        runtime.add_processor(PythonContinuousHostProcessor::node(PythonProcessorConfig {
            project_path: project_path.clone(),
            class_name: "CyberpunkLowerThird".to_string(),
            entry_point: Some("cyberpunk_lower_third.py".to_string()),
        }))?;
    println!("✓ Lower third generator added: {}\n", lower_third);

    // =========================================================================
    // PARALLEL: Watermark (continuous RGBA generator)
    // =========================================================================

    println!("🐍 Adding Python watermark GENERATOR (parallel, 16ms)...");
    let watermark =
        runtime.add_processor(PythonContinuousHostProcessor::node(PythonProcessorConfig {
            project_path: project_path.clone(),
            class_name: "CyberpunkWatermark".to_string(),
            entry_point: Some("cyberpunk_watermark.py".to_string()),
        }))?;
    println!("✓ Watermark generator added: {}\n", watermark);

    // =========================================================================
    // Glitch Effect
    // =========================================================================

    println!("🐍 Adding Python glitch processor (RGB separation, scanlines)...");
    let glitch =
        runtime.add_processor(PythonContinuousHostProcessor::node(PythonProcessorConfig {
            project_path,
            class_name: "CyberpunkGlitch".to_string(),
            entry_point: Some("cyberpunk_glitch.py".to_string()),
        }))?;
    println!("✓ Glitch processor added: {}\n", glitch);

    // =========================================================================
    // DISPLAY: Output
    // =========================================================================

    println!("🖥️  Adding display processor...");
    let display = runtime.add_processor(DisplayProcessor::node(DisplayProcessor::Config {
        width: 1920,
        height: 1080,
        title: Some("Cyberpunk Pipeline (Parallel)".to_string()),
        scaling_mode: Default::default(),
        vsync: false, // Uncapped - run as fast as possible
        ..Default::default()
    }))?;
    println!("✓ Display added: {}\n", display);

    // =========================================================================
    // API Server (for debugging)
    // =========================================================================

    println!("🌐 Adding API server processor...");
    let _api_server = runtime.add_processor(ApiServerProcessor::node(ApiServerConfig {
        host: "127.0.0.1".to_string(),
        port: 9000,
    }))?;
    println!("✓ API server running at http://127.0.0.1:9000");
    println!("   Registry: http://127.0.0.1:9000/registry\n");

    // =========================================================================
    // Connect the Pipeline (PARALLEL)
    // =========================================================================

    println!("🔗 Connecting pipeline (parallel blending)...");

    // Pipeline 1: Camera → Cyberpunk → BlendingCompositor.video_in
    runtime.connect(
        OutputLinkPortRef::new(&camera, "video"),
        InputLinkPortRef::new(&cyberpunk, "video_in"),
    )?;
    println!("   ✓ Camera → Cyberpunk");

    runtime.connect(
        OutputLinkPortRef::new(&cyberpunk, "video_out"),
        InputLinkPortRef::new(&blending, "video_in"),
    )?;
    println!("   ✓ Cyberpunk → BlendingCompositor.video_in");

    // Pipeline 2: LowerThird → BlendingCompositor.lower_third_in
    runtime.connect(
        OutputLinkPortRef::new(&lower_third, "video_out"),
        InputLinkPortRef::new(&blending, "lower_third_in"),
    )?;
    println!("   ✓ LowerThird → BlendingCompositor.lower_third_in");

    // Pipeline 3: Watermark → BlendingCompositor.watermark_in
    runtime.connect(
        OutputLinkPortRef::new(&watermark, "video_out"),
        InputLinkPortRef::new(&blending, "watermark_in"),
    )?;
    println!("   ✓ Watermark → BlendingCompositor.watermark_in");

    // BlendingCompositor → Glitch → Display
    runtime.connect(
        OutputLinkPortRef::new(&blending, "video_out"),
        InputLinkPortRef::new(&glitch, "video_in"),
    )?;
    println!("   ✓ BlendingCompositor → Glitch");

    runtime.connect(
        OutputLinkPortRef::new(&glitch, "video_out"),
        InputLinkPortRef::new(&display, "video"),
    )?;
    println!("   ✓ Glitch → Display");
    println!();

    // =========================================================================
    // Run the pipeline
    // =========================================================================

    println!("▶️  Starting pipeline...");
    println!("   Architecture (parallel blending):");
    println!("     Camera ──→ Cyberpunk ──→ BlendingCompositor ──→ Glitch ──→ Display");
    println!("                                    ↑         ↑");
    println!("     LowerThird (16ms) ─────────────┘         │");
    println!("     Watermark (16ms) ────────────────────────┘");
    println!();
    #[cfg(target_os = "macos")]
    println!("   Press Cmd+Q to stop\n");
    #[cfg(not(target_os = "macos"))]
    println!("   Press Ctrl+C to stop\n");

    runtime.start()?;
    runtime.wait_for_signal()?;

    println!("\n✓ Pipeline stopped gracefully");

    Ok(())
}
