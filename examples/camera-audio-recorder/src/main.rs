use streamlib::{Result, StreamRuntime, request_camera_permission, request_audio_permission};
use streamlib::{CameraProcessor, AudioCaptureProcessor, Mp4WriterProcessor};
use streamlib::core::{CameraConfig, AudioCaptureConfig, Mp4WriterConfig, VideoFrame, AudioFrame};
use std::path::PathBuf;

fn main() -> Result<()> {
    // Initialize tracing
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    println!("=== Camera + Audio → MP4 Recorder Pipeline ===\n");

    // Request camera and microphone permissions (Deno model)
    println!("🔒 Requesting camera permission...");
    if !request_camera_permission()? {
        eprintln!("❌ Camera permission denied!");
        eprintln!("Please grant permission in System Settings → Privacy & Security → Camera");
        return Ok(());
    }
    println!("✅ Camera permission granted\n");

    println!("🔒 Requesting microphone permission...");
    if !request_audio_permission()? {
        eprintln!("❌ Microphone permission denied!");
        eprintln!("Please grant permission in System Settings → Privacy & Security → Microphone");
        return Ok(());
    }
    println!("✅ Microphone permission granted\n");

    let mut runtime = StreamRuntime::new();

    // Get audio config from runtime
    let audio_config = runtime.audio_config();

    // Determine output path
    let output_path = std::env::var("OUTPUT_PATH")
        .unwrap_or_else(|_| "/tmp/recording.mp4".to_string());

    println!("📹 Output file: {}\n", output_path);
    println!("🎵 Audio config: {} Hz, {} channels\n", audio_config.sample_rate, 2);

    println!("📷 Adding camera processor...");
    let camera = runtime.add_processor_with_config::<CameraProcessor>(
        CameraConfig {
            device_id: None, // Use default camera
        }
    )?;
    println!("✓ Camera added (capturing video)\n");

    println!("🎤 Adding audio capture processor...");
    let audio_capture = runtime.add_processor_with_config::<AudioCaptureProcessor>(
        AudioCaptureConfig {
            device_id: None, // Use default microphone
        }
    )?;
    println!("✓ Audio capture added (capturing mono audio @ {} Hz)\n", audio_config.sample_rate);

    println!("💾 Adding MP4 writer processor...");
    let mp4_writer = runtime.add_processor_with_config::<Mp4WriterProcessor>(
        Mp4WriterConfig {
            output_path: PathBuf::from(&output_path),
            sync_tolerance_ms: Some(16.6), // ~1 frame at 60fps
            video_codec: Some("avc1".to_string()), // H.264
            video_bitrate: Some(5_000_000), // 5 Mbps
            audio_codec: Some("aac".to_string()), // AAC (note: currently using LPCM)
            audio_bitrate: Some(128_000), // 128 kbps
        }
    )?;
    println!("✓ MP4 writer added (H.264 video + LPCM audio)\n");

    println!("🔗 Connecting pipeline:");
    println!("   camera.video → mp4_writer.video");
    runtime.connect(
        camera.output_port::<VideoFrame>("video"),
        mp4_writer.input_port::<VideoFrame>("video"),
    )?;
    println!("   ✓ Video connected");

    println!("   audio_capture.audio → mp4_writer.audio");
    runtime.connect(
        audio_capture.output_port::<AudioFrame<2>>("audio"),
        mp4_writer.input_port::<AudioFrame<2>>("audio"),
    )?;
    println!("   ✓ Audio connected\n");

    println!("▶️  Starting recording pipeline...");
    println!("   Recording to: {}", output_path);
    #[cfg(target_os = "macos")]
    println!("   Press Cmd+Q or Ctrl+C to stop recording\n");
    #[cfg(not(target_os = "macos"))]
    println!("   Press Ctrl+C to stop recording\n");

    println!("📊 Pipeline will maintain A/V sync with tolerance of 16.6ms");
    println!("   - Video frames ahead: dropped");
    println!("   - Video frames behind: duplicated\n");

    runtime.start()?;
    runtime.run()?;

    println!("\n✓ Recording stopped");
    println!("✓ MP4 file finalized: {}", output_path);
    println!("\n📊 To view sync statistics, check the logs above");
    println!("💡 Play with: ffplay {} or QuickTime Player", output_path);

    Ok(())
}
