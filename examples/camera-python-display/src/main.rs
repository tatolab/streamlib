// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Camera → Python Grayscale → Display Pipeline Example
//!
//! Demonstrates a full video processing pipeline with a Python-defined
//! grayscale processor in the middle. The Python processor uses a GPU
//! shader for efficient grayscale conversion.
//!
//! Pipeline: Camera → GrayscaleProcessor (Python) → Display
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
    // Initialize tracing with sensible defaults
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
                "info,naga=warn,wgpu_core=warn,wgpu_hal=warn"
                    .parse()
                    .unwrap()
            }),
        )
        .init();

    println!("=== Camera → Python Grayscale → Display Pipeline ===\n");

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
    // Add Python Grayscale processor
    // =========================================================================

    println!("🐍 Adding Python grayscale processor...");

    // Path to the Python project (contains pyproject.toml and grayscale_processor.py)
    let project_path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("python");

    let grayscale =
        runtime.add_processor(PythonHostProcessor::node(PythonHostProcessorConfig {
            project_path,
            class_name: "GrayscaleProcessor".to_string(),
            entry_point: Some("grayscale_processor.py".to_string()),
        }))?;
    println!("✓ Python grayscale processor added: {}\n", grayscale);

    // =========================================================================
    // Add Display processor
    // =========================================================================

    println!("🖥️  Adding display processor...");
    let display = runtime.add_processor(DisplayProcessor::node(DisplayProcessor::Config {
        width: 1920,
        height: 1080,
        title: Some("Camera → Python Grayscale → Display".to_string()),
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
    // Connect the pipeline: Camera → Grayscale → Display
    // =========================================================================

    println!("🔗 Connecting pipeline...");

    // Camera video → Grayscale video_in
    runtime.connect(
        OutputLinkPortRef::new(&camera, "video"),
        InputLinkPortRef::new(&grayscale, "video_in"),
    )?;
    println!("   ✓ Camera → Grayscale");

    // Grayscale video_out → Display video
    runtime.connect(
        OutputLinkPortRef::new(&grayscale, "video_out"),
        InputLinkPortRef::new(&display, "video"),
    )?;
    println!("   ✓ Grayscale → Display");
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
