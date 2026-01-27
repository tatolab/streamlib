// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use streamlib::_generated_::com_tatolab_screen_capture_config::TargetType;
use streamlib::{
    input, output, request_display_permission, Mp4WriterProcessor, Result, ScreenCaptureProcessor,
    StreamRuntime,
};

fn main() -> Result<()> {
    // Initialize tracing
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    println!("=== Screen Recorder Pipeline ===\n");

    // Create runtime first
    let runtime = StreamRuntime::new()?;

    // Request screen recording permission
    println!("Requesting screen recording permission...");
    if !request_display_permission()? {
        eprintln!("Screen recording permission denied!");
        eprintln!(
            "Please grant permission in System Settings -> Privacy & Security -> Screen Recording"
        );
        return Ok(());
    }
    println!("Screen recording permission granted\n");

    // Determine output path
    let output_path = std::env::var("OUTPUT_PATH").unwrap_or_else(|_| {
        let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_| ".".to_string());
        format!("{}/screen_recording.mp4", manifest_dir)
    });

    println!("Output file: {}\n", output_path);

    println!("Adding screen capture processor...");
    let screen_capture = runtime.add_processor(ScreenCaptureProcessor::node(
        ScreenCaptureProcessor::Config {
            target_type: TargetType::Display,
            display_index: Some(0), // Main display
            frame_rate: Some(30.0),
            show_cursor: Some(true),
            exclude_current_app: Some(true),
            ..Default::default()
        },
    ))?;
    println!("Screen capture added (capturing main display)\n");

    println!("Adding MP4 writer processor...");
    let mp4_writer =
        runtime.add_processor(Mp4WriterProcessor::node(Mp4WriterProcessor::Config {
            output_path: output_path.clone(),
            sync_tolerance_ms: Some(16.6),
            video_codec: Some("avc1".to_string()),
            video_bitrate: Some(8_000_000), // 8 Mbps for screen content
            audio_codec: None,
            audio_bitrate: None,
        }))?;
    println!("MP4 writer added (H.264 video)\n");

    println!("Connecting pipeline:");
    println!("   screen_capture.video -> mp4_writer.video");
    runtime.connect(
        output::<ScreenCaptureProcessor::OutputLink::video>(&screen_capture),
        input::<Mp4WriterProcessor::InputLink::video>(&mp4_writer),
    )?;
    println!("   Video connected\n");

    println!("Starting recording pipeline...");
    println!("   Recording to: {}", output_path);
    println!("   Press Ctrl+C to stop recording\n");

    runtime.start()?;
    runtime.wait_for_signal()?;

    println!("\nRecording stopped");
    println!("MP4 file finalized: {}", output_path);
    println!("\nPlay with: ffplay {} or QuickTime Player", output_path);

    Ok(())
}
