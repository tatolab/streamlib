use std::path::PathBuf;
use streamlib::core::{
    AudioCaptureConfig, AudioChannelConverterConfig, AudioFrame, AudioResamplerConfig,
    CameraConfig, ChannelConversionMode, Mp4WriterConfig, ResamplingQuality, VideoFrame,
};
use streamlib::{
    AudioCaptureProcessor, AudioChannelConverterProcessor, AudioResamplerProcessor,
    CameraProcessor, Mp4WriterProcessor,
};
use streamlib::{Result, StreamRuntime};

fn main() -> Result<()> {
    // Initialize tracing
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    println!("=== Camera + Audio → MP4 Recorder Pipeline ===\n");

    // Create runtime first
    let mut runtime = StreamRuntime::new();

    // Request camera and microphone permissions (must be on main thread)
    println!("🔒 Requesting camera permission...");
    if !runtime.request_camera()? {
        eprintln!("❌ Camera permission denied!");
        eprintln!("Please grant permission in System Settings → Privacy & Security → Camera");
        return Ok(());
    }
    println!("✅ Camera permission granted\n");

    println!("🔒 Requesting microphone permission...");
    if !runtime.request_microphone()? {
        eprintln!("❌ Microphone permission denied!");
        eprintln!("Please grant permission in System Settings → Privacy & Security → Microphone");
        return Ok(());
    }
    println!("✅ Microphone permission granted\n");

    // Determine output path - save in example folder by default
    let output_path = std::env::var("OUTPUT_PATH").unwrap_or_else(|_| {
        // Get example directory (parent of Cargo.toml)
        let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_| ".".to_string());
        format!("{}/recording.mp4", manifest_dir)
    });

    println!("📹 Output file: {}\n", output_path);

    println!("📷 Adding camera processor...");
    let camera = runtime.add_processor::<CameraProcessor>(CameraConfig {
        device_id: Some("0x1424001bcf2284".to_string()), // Use default camera
    })?;
    println!("✓ Camera added (capturing video)\n");

    println!("🎤 Adding audio capture processor...");
    let audio_capture =
        runtime.add_processor::<AudioCaptureProcessor>(AudioCaptureConfig {
            device_id: None, // Use default microphone
        })?;
    println!("✓ Audio capture added (mono @ 24kHz)\n");

    println!("🔄 Adding audio resampler (24kHz → 48kHz)...");
    let resampler =
        runtime.add_processor::<AudioResamplerProcessor>(AudioResamplerConfig {
            source_sample_rate: 24000,
            target_sample_rate: 48000,
            quality: ResamplingQuality::High,
        })?;
    println!("✓ Resampler added\n");

    println!("🎛️  Adding channel converter (mono → stereo)...");
    let channel_converter = runtime.add_processor::<AudioChannelConverterProcessor>(
        AudioChannelConverterConfig {
            mode: ChannelConversionMode::Duplicate,
        },
    )?;
    println!("✓ Channel converter added\n");

    println!("💾 Adding MP4 writer processor...");
    let mp4_writer = runtime.add_processor::<Mp4WriterProcessor>(Mp4WriterConfig {
        output_path: PathBuf::from(&output_path),
        sync_tolerance_ms: Some(16.6),         // ~1 frame at 60fps
        video_codec: Some("avc1".to_string()), // H.264
        video_bitrate: Some(5_000_000),        // 5 Mbps
        audio_codec: Some("aac".to_string()),  // AAC (note: currently using LPCM)
        audio_bitrate: Some(128_000),          // 128 kbps
    })?;
    println!("✓ MP4 writer added (H.264 video + stereo LPCM audio @ 48kHz)\n");

    println!("🔗 Connecting pipeline:");
    println!("   camera.video → mp4_writer.video");
    runtime.connect(
        camera.output_port::<VideoFrame>("video"),
        mp4_writer.input_port::<VideoFrame>("video"),
    )?;
    println!("   ✓ Video connected");

    println!("   audio_capture.audio → resampler.audio_in");
    runtime.connect(
        audio_capture.output_port::<AudioFrame>("audio"),
        resampler.input_port::<AudioFrame>("audio_in"),
    )?;
    println!("   ✓ Audio capture → resampler");

    println!("   resampler.audio_out → channel_converter.audio_in");
    runtime.connect(
        resampler.output_port::<AudioFrame>("audio_out"),
        channel_converter.input_port::<AudioFrame>("audio_in"),
    )?;
    println!("   ✓ Resampler → channel converter");

    println!("   channel_converter.audio_out → mp4_writer.audio");
    runtime.connect(
        channel_converter.output_port::<AudioFrame>("audio_out"),
        mp4_writer.input_port::<AudioFrame>("audio"),
    )?;
    println!("   ✓ Channel converter → MP4 writer\n");

    println!("▶️  Starting recording pipeline...");
    println!("   Recording to: {}", output_path);
    println!("   Press Ctrl+C to stop recording\n");

    println!("📊 Audio pipeline: mic (mono 24kHz) → resampler (48kHz) → converter (stereo) → MP4");
    println!("📊 Video pipeline: camera → MP4");
    println!("📊 A/V sync tolerance: 16.6ms (video frames may be dropped/duplicated)\n");

    runtime.start()?;
    runtime.run()?;

    println!("\n✅ Recording stopped");
    println!("✅ MP4 file finalized: {}", output_path);
    println!("\n📊 To view sync statistics, check the logs above");
    println!("💡 Play with: ffplay {} or QuickTime Player", output_path);

    Ok(())
}
