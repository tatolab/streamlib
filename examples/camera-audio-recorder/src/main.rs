// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

#[cfg(not(any(target_os = "macos", target_os = "ios")))]
fn main() {
    eprintln!(
        "camera-audio-recorder currently requires macOS — the Linux MP4 writer \
         does not yet accept an audio input. Tracked as a follow-up to issue #358."
    );
    std::process::exit(2);
}

#[cfg(any(target_os = "macos", target_os = "ios"))]
use streamlib::_generated_::com_tatolab_audio_channel_converter_config::Mode;
#[cfg(any(target_os = "macos", target_os = "ios"))]
use streamlib::_generated_::com_tatolab_audio_resampler_config::Quality;
#[cfg(any(target_os = "macos", target_os = "ios"))]
use streamlib::{
    input, output, request_audio_permission, request_camera_permission, AudioCaptureProcessor,
    AudioChannelConverterProcessor, AudioResamplerProcessor, CameraProcessor, Mp4WriterProcessor,
    Result, StreamRuntime,
};

#[cfg(any(target_os = "macos", target_os = "ios"))]
fn main() -> Result<()> {

    println!("=== Camera + Audio → MP4 Recorder Pipeline ===\n");

    // Create runtime first
    let runtime = StreamRuntime::new()?;

    // Request camera and microphone permissions (must be on main thread)
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

    // Determine output path - save in example folder by default
    let output_path = std::env::var("OUTPUT_PATH").unwrap_or_else(|_| {
        // Get example directory (parent of Cargo.toml)
        let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_| ".".to_string());
        format!("{}/recording.mp4", manifest_dir)
    });

    println!("📹 Output file: {}\n", output_path);

    println!("📷 Adding camera processor...");
    let camera = runtime.add_processor(CameraProcessor::node(CameraProcessor::Config {
        device_id: None, // Use default camera
        ..Default::default()
    }))?;
    println!("✓ Camera added (capturing video)\n");

    println!("🎤 Adding audio capture processor...");
    let audio_capture =
        runtime.add_processor(AudioCaptureProcessor::node(AudioCaptureProcessor::Config {
            device_id: None, // Use default microphone
        }))?;
    println!("✓ Audio capture added (mono @ 24kHz)\n");

    println!("🔄 Adding audio resampler (24kHz → 48kHz)...");
    let resampler = runtime.add_processor(AudioResamplerProcessor::node(
        AudioResamplerProcessor::Config {
            source_sample_rate: 24000,
            target_sample_rate: 48000,
            quality: Quality::High,
        },
    ))?;
    println!("✓ Resampler added\n");

    println!("🎛️  Adding channel converter (mono → stereo)...");
    let channel_converter = runtime.add_processor(AudioChannelConverterProcessor::node(
        AudioChannelConverterProcessor::Config {
            mode: Mode::Duplicate,
            output_channels: None,
        },
    ))?;
    println!("✓ Channel converter added\n");

    println!("💾 Adding MP4 writer processor...");
    let mp4_writer =
        runtime.add_processor(Mp4WriterProcessor::node(Mp4WriterProcessor::Config {
            output_path: output_path.clone(),      // String, not PathBuf
            sync_tolerance_ms: Some(16.6),         // ~1 frame at 60fps
            video_codec: Some("avc1".to_string()), // H.264
            video_bitrate: Some(5_000_000),        // 5 Mbps
            audio_codec: Some("aac".to_string()),  // AAC (note: currently using LPCM)
            audio_bitrate: Some(128_000),          // 128 kbps
        }))?;
    println!("✓ MP4 writer added (H.264 video + stereo LPCM audio @ 48kHz)\n");

    println!("🔗 Connecting pipeline:");

    println!("   camera.video → mp4_writer.video");
    runtime.connect(
        output::<CameraProcessor::OutputLink::video>(&camera),
        input::<Mp4WriterProcessor::InputLink::video>(&mp4_writer),
    )?;
    println!("   ✓ Video connected");

    println!("   audio_capture.audio → resampler.audio_in");
    runtime.connect(
        output::<AudioCaptureProcessor::OutputLink::audio>(&audio_capture),
        input::<AudioResamplerProcessor::InputLink::audio_in>(&resampler),
    )?;
    println!("   ✓ Audio capture → resampler");

    println!("   resampler.audio_out → channel_converter.audio_in");
    runtime.connect(
        output::<AudioResamplerProcessor::OutputLink::audio_out>(&resampler),
        input::<AudioChannelConverterProcessor::InputLink::audio_in>(&channel_converter),
    )?;
    println!("   ✓ Resampler → channel converter");

    println!("   channel_converter.audio_out → mp4_writer.audio");
    runtime.connect(
        output::<AudioChannelConverterProcessor::OutputLink::audio_out>(&channel_converter),
        input::<Mp4WriterProcessor::InputLink::audio>(&mp4_writer),
    )?;
    println!("   ✓ Channel converter → MP4 writer\n");

    println!("▶️  Starting recording pipeline...");
    println!("   Recording to: {}", output_path);
    println!("   Press Ctrl+C to stop recording\n");

    println!("📊 Audio pipeline: mic (mono 24kHz) → resampler (48kHz) → converter (stereo) → MP4");
    println!("📊 Video pipeline: camera → MP4");
    println!("📊 A/V sync tolerance: 16.6ms (video frames may be dropped/duplicated)\n");

    // start() blocks on macOS standalone (runs NSApplication event loop)
    runtime.start()?;

    runtime.wait_for_signal()?;

    println!("\n✅ Recording stopped");
    println!("✅ MP4 file finalized: {}", output_path);
    println!("\n📊 To view sync statistics, check the logs above");
    println!("💡 Play with: ffplay {} or QuickTime Player", output_path);

    Ok(())
}
