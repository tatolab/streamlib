// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::path::PathBuf;
use streamlib::sdk::{CameraConfig, DisplayConfig, Mp4WriterConfig};
use streamlib::sdk::processors::{CameraProcessor, DisplayProcessor, Mp4WriterProcessor};
use streamlib::sdk::error::Result;
use streamlib::sdk::runtime::Runner;
use streamlib::sdk::processors::{input, output};  // TODO: unmapped items

fn main() -> Result<()> {
    let runtime = Runner::new()?;

    // Add a camera processor
    let camera = runtime.add_processor(CameraProcessor::Processor::node(CameraConfig {
        device_id: Some("device-abc-123".to_string()),
        ..Default::default()
    }))?;

    // Add a display processor
    let display = runtime.add_processor(DisplayProcessor::Processor::node(DisplayConfig {
        width: 1920,
        height: 1080,
        title: Some("My Display".to_string()),
        ..Default::default()
    }))?;

    // Add an MP4 writer processor
    let recorder = runtime.add_processor(Mp4WriterProcessor::Processor::node(Mp4WriterConfig {
        output_path: PathBuf::from("/tmp/recording.mp4"),
        video_bitrate: Some(5_000_000),
        audio_bitrate: Some(128_000),
        ..Default::default()
    }))?;

    // Connect camera to display
    runtime.connect(
        output::<CameraProcessor::OutputLink::video>(&camera),
        input::<DisplayProcessor::InputLink::video>(&display),
    )?;

    // Connect camera to recorder (fan-out)
    runtime.connect(
        output::<CameraProcessor::OutputLink::video>(&camera),
        input::<Mp4WriterProcessor::InputLink::video>(&recorder),
    )?;

    // Print the graph as JSON

    let json_str = runtime.to_json().expect("Failed to serialize graph");

    println!("{}", json_str);

    Ok(())
}
