// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Vulkan Video Encode Roundtrip Pipeline
//!
//! Streams a raw BGRA fixture file through the streamlib processor graph:
//!   BgraFileSource → H264/H265 Encoder → LinuxMp4Writer
//!
//! Produces a Telegram-compatible MP4 with silent audio track.
//!
//! Usage:
//!   cargo run -p vulkan-video-roundtrip --release -- h264
//!   cargo run -p vulkan-video-roundtrip --release -- h265

use streamlib::{
    input, output,
    BgraFileSourceProcessor, H264EncoderProcessor, H265EncoderProcessor,
    LinuxMp4WriterProcessor, Result, StreamRuntime,
};

const WIDTH: u32 = 1920;
const HEIGHT: u32 = 1080;
const FPS: u32 = 60;
const DURATION_SECS: u32 = 10;
const FRAME_COUNT: u32 = FPS * DURATION_SECS;

fn main() -> Result<()> {
    let codec = std::env::args().nth(1).unwrap_or_else(|| "h265".into());
    let is_h265 = codec == "h265";

    let fixture_path = format!(
        "{}/libs/vulkan-video/examples/{}-codec/fixtures/smpte_1080p60.bgra",
        env!("CARGO_MANIFEST_DIR").replace("/examples/vulkan-video-roundtrip", ""),
        if is_h265 { "h265" } else { "h264" }
    );

    let output_path = format!("/tmp/streamlib_{codec}_roundtrip.mp4");

    println!("=== Vulkan Video {} Encode Pipeline ===", codec.to_uppercase());
    println!("Source:  {fixture_path}");
    println!("Output:  {output_path}");
    println!("Format:  {WIDTH}x{HEIGHT}@{FPS}fps, {DURATION_SECS}s ({FRAME_COUNT} frames)\n");

    let runtime = StreamRuntime::new()?;

    // --- Source: reads BGRA file frame-by-frame ---
    let source = runtime.add_processor(BgraFileSourceProcessor::node(
        BgraFileSourceProcessor::Config {
            file_path: fixture_path,
            width: WIDTH,
            height: HEIGHT,
            fps: FPS,
            frame_count: FRAME_COUNT,
        },
    ))?;
    println!("+ BgraFileSource: {source}");

    // --- Encoder: Vulkan Video hardware encode ---
    let encoder = if is_h265 {
        runtime.add_processor(H265EncoderProcessor::node(
            H265EncoderProcessor::Config {
                width: Some(WIDTH),
                height: Some(HEIGHT),
                ..Default::default()
            },
        ))?
    } else {
        runtime.add_processor(H264EncoderProcessor::node(
            H264EncoderProcessor::Config {
                width: Some(WIDTH),
                height: Some(HEIGHT),
                ..Default::default()
            },
        ))?
    };
    println!("+ {}Encoder: {encoder}", codec.to_uppercase());

    // --- MP4 Writer: mux encoded stream to MP4 ---
    let mp4_writer = runtime.add_processor(LinuxMp4WriterProcessor::node(
        LinuxMp4WriterProcessor::Config {
            output_path: output_path.clone(),
            fps: FPS,
            codec: Some(if is_h265 { "hevc".into() } else { "h264".into() }),
            duration_secs: Some(DURATION_SECS),
        },
    ))?;
    println!("+ LinuxMp4Writer: {mp4_writer}");

    // --- Wire the pipeline ---
    if is_h265 {
        runtime.connect(
            output::<BgraFileSourceProcessor::OutputLink::video>(&source),
            input::<H265EncoderProcessor::InputLink::video_in>(&encoder),
        )?;
        runtime.connect(
            output::<H265EncoderProcessor::OutputLink::encoded_video_out>(&encoder),
            input::<LinuxMp4WriterProcessor::InputLink::encoded_video_in>(&mp4_writer),
        )?;
    } else {
        runtime.connect(
            output::<BgraFileSourceProcessor::OutputLink::video>(&source),
            input::<H264EncoderProcessor::InputLink::video_in>(&encoder),
        )?;
        runtime.connect(
            output::<H264EncoderProcessor::OutputLink::encoded_video_out>(&encoder),
            input::<LinuxMp4WriterProcessor::InputLink::encoded_video_in>(&mp4_writer),
        )?;
    }
    println!("\nPipeline: source -> encoder -> mp4_writer");

    // --- Run ---
    println!("Starting pipeline...\n");
    runtime.start()?;

    // Wait for the source to finish streaming all frames, plus a buffer
    // for the encoder to flush. The source runs at real-time FPS, so
    // total wall time ≈ DURATION_SECS + encoder flush time.
    let total_wait = std::time::Duration::from_secs(DURATION_SECS as u64 + 5);
    std::thread::sleep(total_wait);

    println!("Source finished, stopping pipeline...");
    runtime.stop()?;

    println!("\nOutput: {output_path}");
    Ok(())
}
