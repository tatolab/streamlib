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

use streamlib::{
    input, output,
    CameraProcessor,
    H264EncoderProcessor, H265EncoderProcessor,
    H264DecoderProcessor, H265DecoderProcessor,
    DisplayProcessor,
    // LinuxMp4WriterProcessor,
    Result, StreamRuntime,
};

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let codec = args.get(1).map(|s| s.as_str()).unwrap_or("h264");
    let device = args.get(2).map(|s| s.as_str()).unwrap_or("/dev/video2");
    let duration_secs: u32 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(30);
    let is_h265 = codec == "h265";

    println!("=== Vulkan Video {} Roundtrip ===", codec.to_uppercase());
    println!("Camera:   {device}");
    println!("Duration: {duration_secs}s\n");

    let runtime = StreamRuntime::new()?;

    // --- Camera ---
    let camera = runtime.add_processor(CameraProcessor::node(CameraProcessor::Config {
        device_id: Some(device.to_string()),
        ..Default::default()
    }))?;
    println!("+ Camera: {camera}");

    // --- Encoder ---
    let encoder = if is_h265 {
        runtime.add_processor(H265EncoderProcessor::node(
            H265EncoderProcessor::Config {
                width: Some(1920),
                height: Some(1080),
                ..Default::default()
            },
        ))?
    } else {
        runtime.add_processor(H264EncoderProcessor::node(
            H264EncoderProcessor::Config {
                width: Some(1920),
                height: Some(1080),
                ..Default::default()
            },
        ))?
    };
    println!("+ {}Encoder: {encoder}", codec.to_uppercase());

    // --- Decoder ---
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

    // --- Display ---
    let display = runtime.add_processor(DisplayProcessor::node(DisplayProcessor::Config {
        width: 1920,
        height: 1080,
        title: Some(format!("streamlib {} Roundtrip", codec.to_uppercase())),
        ..Default::default()
    }))?;
    println!("+ Display: {display}");

    // --- Wire: Camera → Encoder → Decoder → Display ---
    if is_h265 {
        runtime.connect(
            output::<CameraProcessor::OutputLink::video>(&camera),
            input::<H265EncoderProcessor::InputLink::video_in>(&encoder),
        )?;
        runtime.connect(
            output::<H265EncoderProcessor::OutputLink::encoded_video_out>(&encoder),
            input::<H265DecoderProcessor::InputLink::encoded_video_in>(&decoder),
        )?;
        runtime.connect(
            output::<H265DecoderProcessor::OutputLink::video_out>(&decoder),
            input::<DisplayProcessor::InputLink::video>(&display),
        )?;
    } else {
        runtime.connect(
            output::<CameraProcessor::OutputLink::video>(&camera),
            input::<H264EncoderProcessor::InputLink::video_in>(&encoder),
        )?;
        runtime.connect(
            output::<H264EncoderProcessor::OutputLink::encoded_video_out>(&encoder),
            input::<H264DecoderProcessor::InputLink::encoded_video_in>(&decoder),
        )?;
        runtime.connect(
            output::<H264DecoderProcessor::OutputLink::video_out>(&decoder),
            input::<DisplayProcessor::InputLink::video>(&display),
        )?;
    }
    println!("\nPipeline: camera -> encoder -> decoder -> display");

    // --- Run until duration or window close ---
    println!("Starting pipeline for {duration_secs}s...\n");
    runtime.start()?;

    std::thread::sleep(std::time::Duration::from_secs(duration_secs as u64));

    println!("\nStopping pipeline...");
    runtime.stop()?;

    Ok(())
}
