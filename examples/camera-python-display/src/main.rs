// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Camera → Python Cyberpunk → Display Pipeline Example
//!
//! Demonstrates a full video processing pipeline with a Python-defined
//! processor using Skia for GPU-accelerated 2D drawing. Features:
//! - Cyberpunk color grading (teal shadows, magenta highlights)
//! - Spray paint style watermark with drips and neon glow
//! - Zero-copy GPU texture sharing via IOSurface ↔ OpenGL
//!
//! Pipeline: Camera → CyberpunkProcessor (Python/Skia) → Display
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
//!
//! The Rust host will automatically:
//! 1. Create an isolated Python virtual environment
//! 2. Install dependencies from the Python project
//! 3. Inject streamlib-python for the processor decorators
//! 4. Run the Python processor
//! 5. Clean up the venv on shutdown

use std::path::PathBuf;
use streamlib::core::{InputLinkPortRef, OutputLinkPortRef};
use streamlib::{
    ApiServerConfig, ApiServerProcessor, CameraProcessor, DisplayProcessor, Result, StreamRuntime,
};
use streamlib_python::{PythonHostProcessor, PythonHostProcessorConfig};

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
    // Add Python Cyberpunk processor
    // =========================================================================

    println!("🐍 Adding Python cyberpunk processor (Skia GPU rendering)...");

    // Path to the Python project (contains pyproject.toml and cyberpunk_processor.py)
    let project_path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("python");

    let cyberpunk =
        runtime.add_processor(PythonHostProcessor::node(PythonHostProcessorConfig {
            project_path,
            class_name: "CyberpunkProcessor".to_string(),
            entry_point: Some("cyberpunk_processor.py".to_string()),
        }))?;
    println!("✓ Python cyberpunk processor added: {}\n", cyberpunk);

    // =========================================================================
    // Add Display processor
    // =========================================================================

    println!("🖥️  Adding display processor...");
    let display = runtime.add_processor(DisplayProcessor::node(DisplayProcessor::Config {
        width: 1920,
        height: 1080,
        title: Some("Camera → Python Cyberpunk → Display".to_string()),
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
    // Connect the pipeline: Camera → Cyberpunk → Display
    // =========================================================================

    println!("🔗 Connecting pipeline...");

    // Camera video → Cyberpunk video_in
    runtime.connect(
        OutputLinkPortRef::new(&camera, "video"),
        InputLinkPortRef::new(&cyberpunk, "video_in"),
    )?;
    println!("   ✓ Camera → Cyberpunk");

    // Cyberpunk video_out → Display video
    runtime.connect(
        OutputLinkPortRef::new(&cyberpunk, "video_out"),
        InputLinkPortRef::new(&display, "video"),
    )?;
    println!("   ✓ Cyberpunk → Display");
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
