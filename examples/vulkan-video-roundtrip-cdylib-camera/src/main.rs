// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Vulkan Video Encode/Decode Roundtrip Pipeline — camera-as-cdylib variant.
//!
//! Sibling of `examples/vulkan-video-roundtrip` that loads the
//! `@tatolab/camera` processor via `runtime.load_project(...)` (the
//! cdylib plugin path) instead of taking `streamlib-camera` as a
//! Rust dep. Exists to exercise the v12 + v13 + Phase E Slice A
//! (`RhiColorConverterMethodsVTable`) + Phase E Slice B
//! (`RhiCommandRecorderMethodsVTable`) cdylib dispatch end-to-end
//! through the encode → decode → display pipeline.
//!
//!   CameraProcessor (cdylib) → Encoder → Decoder → Display
//!
//! Usage:
//!   cargo run -p vulkan-video-roundtrip-cdylib-camera -- h264 [device] [seconds]
//!   cargo run -p vulkan-video-roundtrip-cdylib-camera -- h265 /dev/video0 10

use streamlib::sdk::error::Result;
use streamlib::sdk::graph::{OutputLinkPortRef, ProcessorUniqueId};
use streamlib::sdk::processors::{input, output, ProcessorSpec};
use streamlib::sdk::runtime::Runner;
use streamlib::sdk::schema_ident;
use streamlib_display::DisplayProcessor;
use streamlib_h264::{H264DecoderProcessor, H264EncoderProcessor};
use streamlib_h265::{H265DecoderProcessor, H265EncoderProcessor};

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let codec = args.get(1).map(|s| s.as_str()).unwrap_or("h264");
    let device = args.get(2).map(|s| s.as_str()).unwrap_or("/dev/video0");
    let duration_secs: u32 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(30);
    let is_h265 = codec == "h265";

    println!(
        "=== Vulkan Video {} Roundtrip (cdylib camera) ===",
        codec.to_uppercase()
    );
    println!("Camera:   {device} (loaded as cdylib via runtime.load_project)");
    println!("Duration: {duration_secs}s\n");

    let runtime = Runner::new()?;

    // 1) Load `@tatolab/camera` (and its `@tatolab/core` dep, walked
    //    via the staged manifest's patch:) from the workspace-staged
    //    location. `cargo xtask build-plugins` must have run first.
    runtime.load_workspace_packages(["@tatolab/camera"])?;
    println!("+ @tatolab/camera loaded from target/streamlib-plugins/");
    println!("+ Wire vocabulary registered (via @tatolab/core dep walk)\n");

    // 3) Camera processor — minted from the cdylib's registration,
    //    addressed by its structured schema_ident, configured via
    //    JSON payload (matches camera_config.yaml).
    let camera_ident = schema_ident!("tatolab", "camera", "Camera", "1.0.0");
    let camera_config = serde_json::json!({
        "device_id": device,
        "max_width": std::env::var("STREAMLIB_CAMERA_MAX_WIDTH")
            .ok()
            .and_then(|s| s.parse::<u32>().ok())
            .unwrap_or(1920),
        "max_height": std::env::var("STREAMLIB_CAMERA_MAX_HEIGHT")
            .ok()
            .and_then(|s| s.parse::<u32>().ok())
            .unwrap_or(1080),
    });
    let camera = runtime.add_processor(ProcessorSpec::new(camera_ident, camera_config))?;
    println!("+ Camera (cdylib): {camera}");

    // 4) Encoder / decoder / display — typed Rust deps as in the
    //    baseline vulkan-video-roundtrip.
    let effort_level: Option<u32> = std::env::var("STREAMLIB_ENCODER_EFFORT_LEVEL")
        .ok()
        .and_then(|s| s.parse().ok());

    let encoder = if is_h265 {
        runtime.add_processor(H265EncoderProcessor::node(
            H265EncoderProcessor::Config {
                effort_level,
                ..Default::default()
            },
        ))?
    } else {
        runtime.add_processor(H264EncoderProcessor::node(
            H264EncoderProcessor::Config {
                effort_level,
                ..Default::default()
            },
        ))?
    };
    println!("+ {}Encoder: {encoder}", codec.to_uppercase());

    let decoder = if is_h265 {
        runtime.add_processor(H265DecoderProcessor::node(
            H265DecoderProcessor::Config::default(),
        ))?
    } else {
        runtime.add_processor(H264DecoderProcessor::node(
            H264DecoderProcessor::Config::default(),
        ))?
    };
    println!("+ {}Decoder: {decoder}", codec.to_uppercase());

    let display = runtime.add_processor(DisplayProcessor::node(DisplayProcessor::Config {
        width: 1920,
        height: 1080,
        title: Some(format!("streamlib {} Roundtrip (cdylib camera)", codec.to_uppercase())),
        ..Default::default()
    }))?;
    println!("+ Display: {display}");

    // 5) Wire — the camera output uses the runtime-typed
    //    `OutputLinkPortRef::new(camera_id, "video")` form since the
    //    cdylib-loaded camera has no compile-time-typed
    //    `CameraProcessor::OutputLink::video` symbol in this crate.
    if is_h265 {
        runtime.connect(
            OutputLinkPortRef::new(&camera, "video"),
            input::<H265EncoderProcessor::InputLink::video_in>(&encoder),
        )?;
        runtime.connect(
            output_typed_h265_encoder(&encoder),
            input::<H265DecoderProcessor::InputLink::encoded_video_in>(&decoder),
        )?;
        runtime.connect(
            output_typed_h265_decoder(&decoder),
            input::<DisplayProcessor::InputLink::video>(&display),
        )?;
    } else {
        runtime.connect(
            OutputLinkPortRef::new(&camera, "video"),
            input::<H264EncoderProcessor::InputLink::video_in>(&encoder),
        )?;
        runtime.connect(
            output_typed_h264_encoder(&encoder),
            input::<H264DecoderProcessor::InputLink::encoded_video_in>(&decoder),
        )?;
        runtime.connect(
            output_typed_h264_decoder(&decoder),
            input::<DisplayProcessor::InputLink::video>(&display),
        )?;
    }
    println!("\nPipeline: camera(cdylib) -> encoder -> decoder -> display");

    println!("Starting pipeline for {duration_secs}s...\n");
    runtime.start()?;

    std::thread::sleep(std::time::Duration::from_secs(duration_secs as u64));

    println!("\nStopping pipeline...");
    runtime.stop()?;

    Ok(())
}

// Small typed-output helpers so the main flow reads symmetrically
// with the runtime-typed camera output.

fn output_typed_h264_encoder(
    encoder: &ProcessorUniqueId,
) -> impl Into<OutputLinkPortRef> {
    output::<H264EncoderProcessor::OutputLink::encoded_video_out>(
        encoder,
    )
}

fn output_typed_h264_decoder(
    decoder: &ProcessorUniqueId,
) -> impl Into<OutputLinkPortRef> {
    output::<H264DecoderProcessor::OutputLink::video_out>(decoder)
}

fn output_typed_h265_encoder(
    encoder: &ProcessorUniqueId,
) -> impl Into<OutputLinkPortRef> {
    output::<H265EncoderProcessor::OutputLink::encoded_video_out>(
        encoder,
    )
}

fn output_typed_h265_decoder(
    decoder: &ProcessorUniqueId,
) -> impl Into<OutputLinkPortRef> {
    output::<H265DecoderProcessor::OutputLink::video_out>(decoder)
}
