use std::path::PathBuf;
use streamlib::core::{CameraConfig, DisplayConfig, Mp4WriterConfig};
use streamlib::{CameraProcessor, DisplayProcessor, Mp4WriterProcessor, Result, StreamRuntime};

fn main() -> Result<()> {
    let mut runtime = StreamRuntime::new();

    // Add a camera processor
    let camera = runtime.add_processor::<CameraProcessor>(CameraConfig {
        device_id: Some("device-abc-123".to_string()),
    })?;

    // Add a display processor
    let display = runtime.add_processor::<DisplayProcessor>(DisplayConfig {
        width: 1920,
        height: 1080,
        title: Some("My Display".to_string()),
        scaling_mode: Default::default(),
    })?;

    // Add an MP4 writer processor
    let recorder = runtime.add_processor::<Mp4WriterProcessor>(Mp4WriterConfig {
        output_path: PathBuf::from("/tmp/recording.mp4"),
        video_bitrate: Some(5_000_000),
        audio_bitrate: Some(128_000),
        ..Default::default()
    })?;

    // Connect camera to display
    runtime.connect(camera.output("video"), display.input("video"))?;

    // Connect camera to recorder (fan-out)
    runtime.connect(camera.output("video"), recorder.input("video"))?;

    // Print the graph as JSON
    let graph = runtime.graph().read();
    let json = serde_json::to_string_pretty(&*graph).expect("Failed to serialize graph");

    println!("{}", json);

    Ok(())
}
