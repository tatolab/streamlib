// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Vulkan Video Encode/Decode Roundtrip Pipeline
//!
//! Captures from a V4L2 camera (vivid virtual device or real camera),
//! encodes via Vulkan Video hardware, decodes back, and displays the
//! decoded frames on screen.
//!
//!   CameraProcessor → Encoder → Decoder → Display
//!
//! Usage:
//!   cargo run -p vulkan-video-roundtrip -- h264 [device] [seconds]
//!   cargo run -p vulkan-video-roundtrip -- h265 /dev/video2 10
//!
//! Packages build automatically on `cargo run` via the build orchestrator.
//!`
//! so the runtime can find each cdylib at load time.

use streamlib::sdk::error::Result;
use streamlib::sdk::graph::{InputLinkPortRef, OutputLinkPortRef};
use streamlib::sdk::module_ident_any_version;
use streamlib::sdk::processors::ProcessorSpec;
use streamlib::sdk::runtime::Runner;
use streamlib::sdk::RunnerAutoBuild;
use streamlib::sdk::schema_ident;

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let codec = args.get(1).map(|s| s.as_str()).unwrap_or("h264");
    let device = args.get(2).map(|s| s.as_str()).unwrap_or("/dev/video2");
    let duration_secs: u32 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(30);
    let is_h265 = codec == "h265";

    println!("=== Vulkan Video {} Roundtrip ===", codec.to_uppercase());
    println!("Camera:   {device}");
    println!("Duration: {duration_secs}s\n");

    let runtime = Runner::with_auto_build()?;

    // Load all four processor packages at runtime. `@tatolab/core` is
    // pulled in transitively by each — its wire-vocabulary schemas
    // (`EncodedVideoFrame.max_payload_bytes` in particular) are
    // load-bearing for iceoryx2 publisher sizing.
    runtime.add_module_with_blocking(module_ident_any_version!("tatolab", "camera"), streamlib::sdk::runtime::Strategy::Path { path: std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../packages/camera"), build: streamlib::sdk::runtime::BuildPolicy::IfStale })?;
    runtime.add_module_with_blocking(module_ident_any_version!("tatolab", "display"), streamlib::sdk::runtime::Strategy::Path { path: std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../packages/display"), build: streamlib::sdk::runtime::BuildPolicy::IfStale })?;
    runtime.add_module_with_blocking(module_ident_any_version!("tatolab", "h264"), streamlib::sdk::runtime::Strategy::Path { path: std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../packages/h264"), build: streamlib::sdk::runtime::BuildPolicy::IfStale })?;
    runtime.add_module_with_blocking(module_ident_any_version!("tatolab", "h265"), streamlib::sdk::runtime::Strategy::Path { path: std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../packages/h265"), build: streamlib::sdk::runtime::BuildPolicy::IfStale })?;

    // --- Camera ---
    // STREAMLIB_CAMERA_MAX_WIDTH / STREAMLIB_CAMERA_MAX_HEIGHT cap V4L2
    // negotiation below the camera's advertised maximum. Useful for
    // exercising non-1080p paths on devices that prefer 1080p.
    let max_width: Option<u32> = std::env::var("STREAMLIB_CAMERA_MAX_WIDTH")
        .ok()
        .and_then(|s| s.parse().ok());
    let max_height: Option<u32> = std::env::var("STREAMLIB_CAMERA_MAX_HEIGHT")
        .ok()
        .and_then(|s| s.parse().ok());
    let mut camera_config = serde_json::Map::new();
    camera_config.insert("device_id".into(), serde_json::Value::String(device.to_string()));
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
    println!("+ Camera: {camera}");

    // --- Encoder ---
    // Optional effort_level override (Vulkan encoder-effort index, not a codec
    // quality setting). Unset → library default for the codec.
    let effort_level: Option<u32> = std::env::var("STREAMLIB_ENCODER_EFFORT_LEVEL")
        .ok()
        .and_then(|s| s.parse().ok());
    let mut encoder_config = serde_json::Map::new();
    if let Some(e) = effort_level {
        encoder_config.insert("effort_level".into(), serde_json::Value::from(e));
    }
    let encoder_ident = if is_h265 {
        schema_ident!("tatolab", "h265", "H265Encoder", "1.0.0")
    } else {
        schema_ident!("tatolab", "h264", "H264Encoder", "1.0.0")
    };
    let encoder = runtime.add_processor(ProcessorSpec::new(
        encoder_ident,
        serde_json::Value::Object(encoder_config),
    ))?;
    println!("+ {}Encoder: {encoder}", codec.to_uppercase());

    // --- Decoder ---
    let decoder_ident = if is_h265 {
        schema_ident!("tatolab", "h265", "H265Decoder", "1.0.0")
    } else {
        schema_ident!("tatolab", "h264", "H264Decoder", "1.0.0")
    };
    let decoder = runtime.add_processor(ProcessorSpec::new(
        decoder_ident,
        serde_json::json!({}),
    ))?;
    println!("+ {}Decoder: {decoder}", codec.to_uppercase());

    // --- Display ---
    let display = runtime.add_processor(ProcessorSpec::new(
        schema_ident!("tatolab", "display", "Display", "1.0.0"),
        serde_json::json!({
            "width": 1920,
            "height": 1080,
            "title": format!("streamlib {} Roundtrip", codec.to_uppercase()),
        }),
    ))?;
    println!("+ Display: {display}");

    // --- Wire: Camera → Encoder → Decoder → Display ---
    runtime.connect(
        OutputLinkPortRef::new(&camera, "video"),
        InputLinkPortRef::new(&encoder, "video_in"),
    )?;
    runtime.connect(
        OutputLinkPortRef::new(&encoder, "encoded_video_out"),
        InputLinkPortRef::new(&decoder, "encoded_video_in"),
    )?;
    runtime.connect(
        OutputLinkPortRef::new(&decoder, "video_out"),
        InputLinkPortRef::new(&display, "video"),
    )?;
    println!("\nPipeline: camera -> encoder -> decoder -> display");

    // --- Run until duration or window close ---
    println!("Starting pipeline for {duration_secs}s...\n");
    runtime.start()?;

    std::thread::sleep(std::time::Duration::from_secs(duration_secs as u64));

    println!("\nStopping pipeline...");
    runtime.stop()?;

    Ok(())
}
