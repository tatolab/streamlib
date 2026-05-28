// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Camera → Display Pipeline Example
//!
//! Loads `@tatolab/camera`, `@tatolab/display`, and `@tatolab/api-server`
//! at runtime, wires camera → display, and exposes the runtime's REST
//! API on `http://127.0.0.1:9000`.
//!
//! Packages build automatically on `cargo run` via the build orchestrator.
//!` so the
//! runtime can resolve each cdylib at load time.

use streamlib::sdk::error::Result;
use streamlib::sdk::graph::{InputLinkPortRef, OutputLinkPortRef};
use streamlib::sdk::module_ident_any_version;
use streamlib::sdk::processors::ProcessorSpec;
use streamlib::sdk::runtime::Runner;
use streamlib::sdk::RunnerAutoBuild;
use streamlib::sdk::schema_ident;

fn main() -> Result<()> {
    println!("=== Camera → Display Pipeline ===\n");

    let runtime = Runner::with_auto_build()?;

    runtime.add_module_with_blocking(module_ident_any_version!("tatolab", "camera"), streamlib::sdk::runtime::Strategy::Path { path: std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../packages/camera"), build: streamlib::sdk::runtime::BuildPolicy::IfStale })?;
    runtime.add_module_with_blocking(module_ident_any_version!("tatolab", "display"), streamlib::sdk::runtime::Strategy::Path { path: std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../packages/display"), build: streamlib::sdk::runtime::BuildPolicy::IfStale })?;
    runtime.add_module_with_blocking(module_ident_any_version!("tatolab", "api-server"), streamlib::sdk::runtime::Strategy::Path { path: std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../packages/api-server"), build: streamlib::sdk::runtime::BuildPolicy::IfStale })?;

    println!("📷 Adding camera processor...");
    let device_id = std::env::var("STREAMLIB_CAMERA_DEVICE").ok();
    let max_width: Option<u32> = std::env::var("STREAMLIB_CAMERA_MAX_WIDTH")
        .ok()
        .and_then(|s| s.parse().ok());
    let max_height: Option<u32> = std::env::var("STREAMLIB_CAMERA_MAX_HEIGHT")
        .ok()
        .and_then(|s| s.parse().ok());
    let mut camera_config = serde_json::Map::new();
    if let Some(id) = device_id {
        camera_config.insert("device_id".into(), serde_json::Value::String(id));
    }
    if let Some(w) = max_width {
        camera_config.insert("max_width".into(), serde_json::Value::from(w));
    }
    if let Some(h) = max_height {
        camera_config.insert("max_height".into(), serde_json::Value::from(h));
    }
    let camera = runtime.add_processor(ProcessorSpec::new(
        schema_ident!("tatolab", "camera", "Camera", "1.0.0"),
        serde_json::Value::Object(camera_config),
    ))?;
    println!("✓ Camera added: {}\n", camera);

    println!("🖥️  Adding display processor...");
    let display = runtime.add_processor(ProcessorSpec::new(
        schema_ident!("tatolab", "display", "Display", "1.0.0"),
        serde_json::json!({
            "width": 1920,
            "height": 1080,
            "title": "streamlib Camera Display",
        }),
    ))?;
    println!("✓ Display added: {}\n", display);

    println!("🌐 Adding API server processor...");
    runtime.add_processor(ProcessorSpec::new(
        schema_ident!("tatolab", "api-server", "ApiServer", "1.0.0"),
        serde_json::json!({
            "host": "127.0.0.1",
            "port": 9000,
        }),
    ))?;
    println!("✓ API server at http://127.0.0.1:9000\n");

    println!("🔗 Connecting camera → display...");
    runtime.connect(
        OutputLinkPortRef::new(&camera, "video"),
        InputLinkPortRef::new(&display, "video"),
    )?;
    println!("✓ Pipeline connected\n");

    println!("▶️  Starting pipeline...");
    #[cfg(target_os = "macos")]
    println!("   Press Cmd+Q to stop\n");
    #[cfg(not(target_os = "macos"))]
    println!("   Press Ctrl+C to stop\n");

    runtime.start()?;
    runtime.wait_for_signal()?;

    println!("\n⏹️  Stopping...");
    println!("✓ Stopped");
    Ok(())
}
