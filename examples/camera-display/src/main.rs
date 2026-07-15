// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Camera → Display Pipeline Example
//!
//! Wires `@tatolab/camera` → `@tatolab/display`.
//!
//! There is no module-loading call in this app: every processor's package
//! lives in this app's `streamlib_modules/` folder (populated by
//! `./setup.sh`), and the runtime lazily discovers + loads each one on the
//! first `processor_type_ref!` reference. The reference sites carry no
//! version — `processor_type_ref!` resolves to the installed provider.

use streamlib::sdk::RunnerAutoBuild;
use streamlib::sdk::error::Result;
use streamlib::sdk::graph::{InputLinkPortRef, OutputLinkPortRef};
use streamlib::sdk::processor_type_ref;
use streamlib::sdk::processors::ProcessorSpec;
use streamlib::sdk::runtime::Runner;

fn main() -> Result<()> {
    println!("=== Camera → Display Pipeline ===\n");

    let runtime = Runner::with_auto_build()?;

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
        processor_type_ref!("tatolab", "camera", "Camera"),
        serde_json::Value::Object(camera_config),
    ))?;
    println!("✓ Camera added: {}\n", camera);

    println!("🖥️  Adding display processor...");
    let display = runtime.add_processor(ProcessorSpec::new(
        processor_type_ref!("tatolab", "display", "Display"),
        serde_json::json!({
            "width": 1920,
            "height": 1080,
            "title": "streamlib Camera Display",
        }),
    ))?;
    println!("✓ Display added: {}\n", display);

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
