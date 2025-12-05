// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::path::PathBuf;
use streamlib::core::{CameraConfig, DisplayConfig, Mp4WriterConfig};
use streamlib::{
    input, output, CameraProcessor, DisplayProcessor, Mp4WriterProcessor, Result, StreamRuntime,
};

fn main() -> Result<()> {
    let mut runtime = StreamRuntime::new();

    // Add a camera processor
    let camera = runtime.add_processor::<CameraProcessor::Processor>(CameraConfig {
        device_id: Some("device-abc-123".to_string()),
    })?;

    // Add a display processor
    let display = runtime.add_processor::<DisplayProcessor::Processor>(DisplayConfig {
        width: 1920,
        height: 1080,
        title: Some("My Display".to_string()),
        scaling_mode: Default::default(),
    })?;

    // Add an MP4 writer processor
    let recorder = runtime.add_processor::<Mp4WriterProcessor::Processor>(Mp4WriterConfig {
        output_path: PathBuf::from("/tmp/recording.mp4"),
        video_bitrate: Some(5_000_000),
        audio_bitrate: Some(128_000),
        ..Default::default()
    })?;

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
    let graph = runtime.graph().read();
    let json = graph.to_json();
    let json_str = serde_json::to_string_pretty(&json).expect("Failed to serialize graph");

    println!("{}", json_str);

    Ok(())
}
