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

use streamlib::{
    input, output, BgraFileSourceProcessor, DisplayProcessor, H264DecoderProcessor,
    H264EncoderProcessor, H265DecoderProcessor, H265EncoderProcessor, Result, StreamRuntime,
};

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

    let runtime = StreamRuntime::new()?;

    let source = runtime.add_processor(BgraFileSourceProcessor::node(
        BgraFileSourceProcessor::Config {
            file_path: bgra_path,
            width,
            height,
            fps,
            frame_count,
        },
    ))?;
    println!("+ BgraFileSource: {source}");

    // Optional quality_level override (used by the #306 PSNR-sweep harness to
    // pick the real-time default). `STREAMLIB_ENCODER_QUALITY_LEVEL` unset →
    // library default for the codec.
    let quality_level: Option<u32> = std::env::var("STREAMLIB_ENCODER_QUALITY_LEVEL")
        .ok()
        .and_then(|s| s.parse().ok());

    let encoder = if is_h265 {
        runtime.add_processor(H265EncoderProcessor::node(H265EncoderProcessor::Config {
            width: Some(width),
            height: Some(height),
            quality_level,
            ..Default::default()
        }))?
    } else {
        runtime.add_processor(H264EncoderProcessor::node(H264EncoderProcessor::Config {
            width: Some(width),
            height: Some(height),
            quality_level,
            ..Default::default()
        }))?
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
        width,
        height,
        title: Some(format!("streamlib {} PSNR rig", codec.to_uppercase())),
        ..Default::default()
    }))?;
    println!("+ Display: {display}");

    if is_h265 {
        runtime.connect(
            output::<BgraFileSourceProcessor::OutputLink::video>(&source),
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
            output::<BgraFileSourceProcessor::OutputLink::video>(&source),
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
