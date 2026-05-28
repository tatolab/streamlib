// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Vulkan Video Encode/Decode Roundtrip Pipeline — camera-as-cdylib variant.
//!
//! Sibling of `examples/vulkan-video-roundtrip` that loads every
//! processor as a cdylib via `runtime.add_module_blocking(...)`.
//! Exists to exercise the cdylib FFI surface (Phase E
//! `RhiColorConverterMethodsVTable` + `RhiCommandRecorderMethodsVTable`)
//! end-to-end through the encode → decode → display pipeline.
//!
//!   CameraProcessor (cdylib) → Encoder (cdylib) → Decoder (cdylib) → Display (cdylib)
//!
//! Usage:
//!   cargo run -p vulkan-video-roundtrip-cdylib-camera -- h264 [device] [seconds]
//!   cargo run -p vulkan-video-roundtrip-cdylib-camera -- h265 /dev/video0 10
//!
//! Run prerequisite: `cargo xtask build-plugins --package @tatolab/camera
//! --package @tatolab/display --package @tatolab/h264 --package @tatolab/h265`
//! so the runtime can resolve each cdylib at load time.

use streamlib::sdk::error::Result;
use streamlib::sdk::graph::{InputLinkPortRef, OutputLinkPortRef};
use streamlib::sdk::module_ident_any_version;
use streamlib::sdk::processors::ProcessorSpec;
use streamlib::sdk::runtime::Runner;
use streamlib::sdk::schema_ident;

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let codec = args.get(1).map(|s| s.as_str()).unwrap_or("h264");
    let device = args.get(2).map(|s| s.as_str()).unwrap_or("/dev/video0");
    let duration_secs: u32 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(30);
    let is_h265 = codec == "h265";

    println!(
        "=== Vulkan Video {} Roundtrip (all-cdylib) ===",
        codec.to_uppercase()
    );
    println!("Camera:   {device} (loaded as cdylib via add_module)");
    println!("Duration: {duration_secs}s\n");

    let runtime = Runner::new_with_orchestrator(streamlib::sdk::PolyglotBuildOrchestrator::default())?;

    runtime.add_module_with_blocking(module_ident_any_version!("tatolab", "camera"), streamlib::sdk::runtime::Strategy::Path { path: std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../packages/camera"), build: streamlib::sdk::runtime::BuildPolicy::IfStale })?;
    runtime.add_module_with_blocking(module_ident_any_version!("tatolab", "display"), streamlib::sdk::runtime::Strategy::Path { path: std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../packages/display"), build: streamlib::sdk::runtime::BuildPolicy::IfStale })?;
    runtime.add_module_with_blocking(module_ident_any_version!("tatolab", "h264"), streamlib::sdk::runtime::Strategy::Path { path: std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../packages/h264"), build: streamlib::sdk::runtime::BuildPolicy::IfStale })?;
    runtime.add_module_with_blocking(module_ident_any_version!("tatolab", "h265"), streamlib::sdk::runtime::Strategy::Path { path: std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../packages/h265"), build: streamlib::sdk::runtime::BuildPolicy::IfStale })?;
    println!("+ Camera / Display / H264 / H265 loaded from target/streamlib-plugins/");
    println!("+ Wire vocabulary registered (via @tatolab/core dep walk)\n");

    let camera = runtime.add_processor(ProcessorSpec::new(
        schema_ident!("tatolab", "camera", "Camera", "1.0.0"),
        serde_json::json!({
            "device_id": device,
            "max_width": std::env::var("STREAMLIB_CAMERA_MAX_WIDTH")
                .ok()
                .and_then(|s| s.parse::<u32>().ok())
                .unwrap_or(1920),
            "max_height": std::env::var("STREAMLIB_CAMERA_MAX_HEIGHT")
                .ok()
                .and_then(|s| s.parse::<u32>().ok())
                .unwrap_or(1080),
        }),
    ))?;
    println!("+ Camera (cdylib): {camera}");

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

    let display = runtime.add_processor(ProcessorSpec::new(
        schema_ident!("tatolab", "display", "Display", "1.0.0"),
        serde_json::json!({
            "width": 1920,
            "height": 1080,
            "title": format!("streamlib {} Roundtrip (all-cdylib)", codec.to_uppercase()),
        }),
    ))?;
    println!("+ Display: {display}");

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
    println!("\nPipeline: camera(cdylib) -> encoder(cdylib) -> decoder(cdylib) -> display(cdylib)");

    println!("Starting pipeline for {duration_secs}s...\n");
    runtime.start()?;

    std::thread::sleep(std::time::Duration::from_secs(duration_secs as u64));

    println!("\nStopping pipeline...");
    runtime.stop()?;

    Ok(())
}
