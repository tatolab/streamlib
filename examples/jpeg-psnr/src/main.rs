// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Fixture-driven JPEG decode PSNR rig (issue #844).
//!
//! Feeds a single JPEG file through `JpegBytesSource → JpegDecoder →
//! Display` so the decoded PNG sampler can capture the decoded frame
//! for external PSNR comparison against the reference PNG that
//! produced the JPEG.
//!
//! Usage:
//!   jpeg-psnr <jpeg-path> <width> <height> <fps> <frame-count>

use streamlib::sdk::error::Result;
use streamlib::sdk::processors::{input, output};
use streamlib::sdk::runtime::Runner;
use streamlib_debug_utilities::JpegBytesSourceProcessor;
use streamlib_display::DisplayProcessor;
use streamlib_jpeg::JpegDecoderProcessor;

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let jpeg_path = args
        .get(1)
        .cloned()
        .expect("missing <jpeg-path>: usage jpeg-psnr <jpeg-path> <w> <h> <fps> <frames>");
    let width: u32 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(1920);
    let height: u32 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(1080);
    let fps: u32 = args.get(4).and_then(|s| s.parse().ok()).unwrap_or(10);
    let frame_count: u32 = args.get(5).and_then(|s| s.parse().ok()).unwrap_or(15);

    println!("=== JPEG decode PSNR rig ===");
    println!("Fixture: {jpeg_path}");
    println!("Format:  {width}x{height} @ {fps}fps, {frame_count} frames\n");

    let runtime = Runner::new()?;

    runtime.load_project(env!("CARGO_MANIFEST_DIR"))?;

    let source = runtime.add_processor(JpegBytesSourceProcessor::node(
        JpegBytesSourceProcessor::Config {
            file_path: jpeg_path,
            fps: Some(fps),
            frame_count: Some(frame_count),
        },
    ))?;
    println!("+ JpegBytesSource: {source}");

    let decoder = runtime.add_processor(JpegDecoderProcessor::node(
        JpegDecoderProcessor::Config {
            max_width: Some(width.max(1)),
            max_height: Some(height.max(1)),
        },
    ))?;
    println!("+ JpegDecoder: {decoder}");

    let display = runtime.add_processor(DisplayProcessor::node(DisplayProcessor::Config {
        width,
        height,
        title: Some("streamlib JPEG PSNR rig".to_string()),
        ..Default::default()
    }))?;
    println!("+ Display: {display}");

    runtime.connect(
        output::<JpegBytesSourceProcessor::OutputLink::encoded_jpeg>(&source),
        input::<JpegDecoderProcessor::InputLink::encoded_jpeg_in>(&decoder),
    )?;
    runtime.connect(
        output::<JpegDecoderProcessor::OutputLink::video_out>(&decoder),
        input::<DisplayProcessor::InputLink::video>(&display),
    )?;
    println!("\nPipeline: jpeg_bytes_source -> jpeg_decoder -> display\n");

    // Source emits `frame_count` JPEGs at `fps` then stops (its
    // background thread exits). Sleep covers the source's emit window
    // plus a small tail for decoder GPU dispatch + display draws.
    let seconds = frame_count / fps.max(1) + 3;
    println!("Starting pipeline for ~{seconds}s...\n");
    runtime.start()?;
    std::thread::sleep(std::time::Duration::from_secs(seconds as u64));
    println!("\nStopping pipeline...");
    runtime.stop()?;

    Ok(())
}
