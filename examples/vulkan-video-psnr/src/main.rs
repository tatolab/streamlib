// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Fixture-driven encode/decode PSNR rig (issue #305).
//!
//! Feeds a pre-built raw BGRA file through BgraFileSource → encoder →
//! decoder → display so the decoded PNG sampler can pair each decoded
//! frame with its reference input (via the carried frame index) and an
//! external harness can compute PSNR against a known ground truth.
//!
//! Usage:
//!   vulkan-video-psnr <h264|h265> <bgra-path> <width> <height> <fps> <frame-count>
//!
//! Packages build automatically on `cargo run` via the build orchestrator.
//!`
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
    let bgra_path = args
        .get(2)
        .cloned()
        .expect("missing <bgra-path>: usage vulkan-video-psnr <codec> <bgra-path> <w> <h> <fps> <frames>");
    let width: u32 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(1920);
    let height: u32 = args.get(4).and_then(|s| s.parse().ok()).unwrap_or(1080);
    let fps: u32 = args.get(5).and_then(|s| s.parse().ok()).unwrap_or(30);
    let frame_count: u32 = args.get(6).and_then(|s| s.parse().ok()).unwrap_or(90);
    let is_h265 = codec == "h265";

    println!("=== Vulkan Video {} PSNR rig ===", codec.to_uppercase());
    println!("Fixture: {bgra_path}");
    println!("Format:  {width}x{height} @ {fps}fps, {frame_count} frames\n");

    let runtime = Runner::new_with_orchestrator(streamlib::sdk::PolyglotBuildOrchestrator::default())?;

    runtime.add_module_with_blocking(module_ident_any_version!("tatolab", "debug-utilities"), streamlib::sdk::runtime::Strategy::Path { path: std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../packages/debug-utilities"), build: streamlib::sdk::runtime::BuildPolicy::IfStale })?;
    runtime.add_module_with_blocking(module_ident_any_version!("tatolab", "display"), streamlib::sdk::runtime::Strategy::Path { path: std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../packages/display"), build: streamlib::sdk::runtime::BuildPolicy::IfStale })?;
    runtime.add_module_with_blocking(module_ident_any_version!("tatolab", "h264"), streamlib::sdk::runtime::Strategy::Path { path: std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../packages/h264"), build: streamlib::sdk::runtime::BuildPolicy::IfStale })?;
    runtime.add_module_with_blocking(module_ident_any_version!("tatolab", "h265"), streamlib::sdk::runtime::Strategy::Path { path: std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../packages/h265"), build: streamlib::sdk::runtime::BuildPolicy::IfStale })?;

    let source = runtime.add_processor(ProcessorSpec::new(
        schema_ident!("tatolab", "debug-utilities", "BgraFileSource", "1.0.0"),
        serde_json::json!({
            "file_path": bgra_path,
            "width": width,
            "height": height,
            "fps": fps,
            "frame_count": frame_count,
        }),
    ))?;
    println!("+ BgraFileSource: {source}");

    // Optional effort_level override (Vulkan encoder-effort index — not a codec
    // quality setting). Used by the #306 sweep harness; unset → library default.
    let effort_level: Option<u32> = std::env::var("STREAMLIB_ENCODER_EFFORT_LEVEL")
        .ok()
        .and_then(|s| s.parse().ok());
    let mut encoder_config = serde_json::Map::new();
    encoder_config.insert("width".into(), serde_json::Value::from(width));
    encoder_config.insert("height".into(), serde_json::Value::from(height));
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
            "width": width,
            "height": height,
            "title": format!("streamlib {} PSNR rig", codec.to_uppercase()),
        }),
    ))?;
    println!("+ Display: {display}");

    runtime.connect(
        OutputLinkPortRef::new(&source, "video"),
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
    println!("\nPipeline: bgra_file_source -> encoder -> decoder -> display\n");

    // Frame throttling in BgraFileSource is real-time, so the run length is
    // (frame_count / fps) plus a small tail so the last frames flush through
    // encode/decode/display.
    let seconds = frame_count / fps.max(1) + 3;
    println!("Starting pipeline for ~{seconds}s...\n");
    runtime.start()?;
    std::thread::sleep(std::time::Duration::from_secs(seconds as u64));
    println!("\nStopping pipeline...");
    runtime.stop()?;

    Ok(())
}
