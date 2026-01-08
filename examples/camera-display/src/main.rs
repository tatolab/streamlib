// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Camera → Display Pipeline Example
//!
//! Demonstrates both typed and string-based processor creation APIs.
//!
//! ## Usage
//!
//! **Typed mode (default)** - compile-time type safety:
//! ```bash
//! cargo run -p camera-display
//! ```
//!
//! **String mode** - REST API style with string names and JSON configs:
//! ```bash
//! cargo run -p camera-display -- --string-mode
//! ```

use streamlib::core::{InputLinkPortRef, OutputLinkPortRef};
use streamlib::{
    input, output, CameraProcessor, DisplayProcessor, ProcessorSpec, Result, StreamRuntime,
};

fn main() -> Result<()> {
    // Initialize tracing with sensible defaults (silence noisy GPU crates)
    // Override with RUST_LOG env var if needed, e.g., RUST_LOG=trace
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
                "debug,naga=warn,wgpu_core=warn,wgpu_hal=warn"
                    .parse()
                    .unwrap()
            }),
        )
        .init();

    // Check for --string-mode argument
    let use_string_mode = std::env::args().any(|arg| arg == "--string-mode");

    if use_string_mode {
        println!("=== Camera → Display Pipeline (STRING MODE) ===\n");
        println!("This mode simulates REST API usage with string-based processor names.\n");
        run_string_mode()
    } else {
        println!("=== Camera → Display Pipeline (TYPED MODE) ===\n");
        println!("Use --string-mode to test REST API style string-based creation.\n");
        run_typed_mode()
    }
}

/// Typed mode - uses compile-time type safety with ::node() methods
fn run_typed_mode() -> Result<()> {
    let runtime = StreamRuntime::new()?;

    // =========================================================================
    // Add processors using typed API
    // =========================================================================

    println!("📷 Adding camera processor...");
    let camera = runtime.add_processor(CameraProcessor::node(CameraProcessor::Config {
        device_id: None,
        ..Default::default()
    }))?;
    println!("✓ Camera added: {}\n", camera);

    println!("🖥️  Adding display processor...");
    let display = runtime.add_processor(DisplayProcessor::node(DisplayProcessor::Config {
        width: 1920,
        height: 1080,
        title: Some("streamlib Camera Display".to_string()),
        scaling_mode: Default::default(),
        ..Default::default()
    }))?;
    println!("✓ Display added: {}\n", display);

    // =========================================================================
    // Connect ports using typed API
    // =========================================================================

    println!("🔗 Connecting camera → display...");
    runtime.connect(
        output::<CameraProcessor::OutputLink::video>(&camera),
        input::<DisplayProcessor::InputLink::video>(&display),
    )?;
    println!("✓ Pipeline connected\n");

    // =========================================================================
    // Run the pipeline
    // =========================================================================

    run_pipeline(runtime)
}

/// String mode - simulates REST API with string-based processor names and JSON configs
fn run_string_mode() -> Result<()> {
    let runtime = StreamRuntime::new()?;

    // =========================================================================
    // Add processors using string-based API (REST API style)
    // =========================================================================

    // Simulate receiving JSON from REST API:
    // { "processor": "CameraProcessor", "config": { "device_id": null } }
    println!("📷 Adding camera processor (string mode)...");
    let camera_spec = ProcessorSpec::new(
        "CameraProcessor",
        serde_json::json!({
            "device_id": null
        }),
    );
    let camera = runtime.add_processor(camera_spec)?;
    println!("✓ Camera added: {}\n", camera);

    // Simulate receiving JSON from REST API:
    // { "processor": "DisplayProcessor", "config": { "width": 3840, "height": 2160, ... } }
    println!("🖥️  Adding display processor (string mode)...");
    let display_spec = ProcessorSpec::new(
        "DisplayProcessor",
        serde_json::json!({
            "width": 1920,
            "height": 1080,
            "title": "streamlib Camera Display (String Mode)",
            "scaling_mode": "Stretch"
        }),
    );
    let display = runtime.add_processor(display_spec)?;
    println!("✓ Display added: {}\n", display);

    // =========================================================================
    // Connect ports using string-based API (REST API style)
    // =========================================================================

    // Simulate receiving JSON from REST API:
    // { "from": { "processor_id": "...", "port": "video" }, "to": { "processor_id": "...", "port": "video" } }
    println!("🔗 Connecting camera → display (string mode)...");
    runtime.connect(
        OutputLinkPortRef::new(&camera, "video"),
        InputLinkPortRef::new(&display, "video"),
    )?;
    println!("✓ Pipeline connected\n");

    // =========================================================================
    // Run the pipeline
    // =========================================================================

    run_pipeline(runtime)
}

fn run_pipeline(runtime: std::sync::Arc<StreamRuntime>) -> Result<()> {
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
