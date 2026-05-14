// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

#[allow(non_snake_case, unused_imports, dead_code, clippy::all)]
mod _generated_ {
    include!(concat!(env!("OUT_DIR"), "/_generated_shim.rs"));
}

#[cfg(not(any(target_os = "macos", target_os = "ios")))]
fn main() {
    eprintln!(
        "camera-audio-recorder currently requires macOS — the Linux MP4 writer \
         does not yet accept an audio input. Tracked as a follow-up to issue #358."
    );
    std::process::exit(2);
}

#[cfg(any(target_os = "macos", target_os = "ios"))]
use streamlib::sdk::permissions::{request_audio_permission, request_camera_permission};
#[cfg(any(target_os = "macos", target_os = "ios"))]
use streamlib::sdk::processors::Mp4WriterProcessor;
#[cfg(any(target_os = "macos", target_os = "ios"))]
use streamlib_camera::CameraProcessor;
#[cfg(any(target_os = "macos", target_os = "ios"))]
use streamlib::sdk::error::Result;
#[cfg(any(target_os = "macos", target_os = "ios"))]
use streamlib::sdk::runtime::Runner;
#[cfg(any(target_os = "macos", target_os = "ios"))]
use streamlib::sdk::processors::{input, output, ProcessorSpec};
#[cfg(any(target_os = "macos", target_os = "ios"))]
use streamlib::sdk::schema_ident_any_version;
#[cfg(any(target_os = "macos", target_os = "ios"))]
use streamlib_audio::{
    AudioCaptureProcessor, AudioChannelConverterProcessor, AudioResamplerProcessor,
};

#[cfg(any(target_os = "macos", target_os = "ios"))]
use crate::_generated_::tatolab__camera::CameraConfig;
#[cfg(any(target_os = "macos", target_os = "ios"))]
use crate::_generated_::tatolab__audio::{
    AudioCaptureConfig, AudioChannelConverterConfig, AudioResamplerConfig,
};
#[cfg(any(target_os = "macos", target_os = "ios"))]
use crate::_generated_::tatolab__audio::audio_channel_converter_config::Mode;
#[cfg(any(target_os = "macos", target_os = "ios"))]
use crate::_generated_::tatolab__audio::audio_resampler_config::Quality;

#[cfg(any(target_os = "macos", target_os = "ios"))]
fn main() -> Result<()> {
    println!("=== Camera + Audio → MP4 Recorder Pipeline ===\n");

    let runtime = Runner::new()?;

    println!("🔒 Requesting camera permission...");
    if !request_camera_permission()? {
        eprintln!("❌ Camera permission denied!");
        return Ok(());
    }
    println!("✅ Camera permission granted\n");

    println!("🔒 Requesting microphone permission...");
    if !request_audio_permission()? {
        eprintln!("❌ Microphone permission denied!");
        return Ok(());
    }
    println!("✅ Microphone permission granted\n");

    let output_path = std::env::var("OUTPUT_PATH").unwrap_or_else(|_| {
        let manifest_dir =
            std::env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_| ".".to_string());
        format!("{}/recording.mp4", manifest_dir)
    });

    println!("📹 Output file: {}\n", output_path);

    let camera = runtime.add_processor(ProcessorSpec::new(
        schema_ident_any_version!("tatolab", "camera", "Camera")?,
        serde_json::to_value(CameraConfig::default())
            .map_err(|e| streamlib::sdk::error::Error::Configuration(e.to_string()))?,
    ))?;

    let audio_capture = runtime.add_processor(ProcessorSpec::new(
        schema_ident_any_version!("tatolab", "audio", "AudioCapture")?,
        serde_json::to_value(AudioCaptureConfig { device_id: None })
            .map_err(|e| streamlib::sdk::error::Error::Configuration(e.to_string()))?,
    ))?;

    let resampler = runtime.add_processor(ProcessorSpec::new(
        schema_ident_any_version!("tatolab", "audio", "AudioResampler")?,
        serde_json::to_value(AudioResamplerConfig {
            source_sample_rate: 24000,
            target_sample_rate: 48000,
            quality: Quality::High,
        })
        .map_err(|e| streamlib::sdk::error::Error::Configuration(e.to_string()))?,
    ))?;

    let channel_converter = runtime.add_processor(ProcessorSpec::new(
        schema_ident_any_version!("tatolab", "audio", "AudioChannelConverter")?,
        serde_json::to_value(AudioChannelConverterConfig {
            mode: Mode::Duplicate,
            output_channels: None,
        })
        .map_err(|e| streamlib::sdk::error::Error::Configuration(e.to_string()))?,
    ))?;

    // Mp4Writer on macOS comes from a not-yet-activated Apple impl
    // (`_apple_impl_pending_`); its schema isn't declared in this
    // example's streamlib.yaml. Pass the config as a `serde_json::Value`
    // literal — the wire-shape is JSON anyway and the typed Rust
    // dataclass adds nothing here.
    let mp4_writer = runtime.add_processor(ProcessorSpec::new(
        schema_ident_any_version!("tatolab", "mp4", "Mp4Writer")?,
        serde_json::json!({
            "output_path": output_path,
            "sync_tolerance_ms": 16.6,
            "video_codec": "avc1",
            "video_bitrate": 5_000_000,
            "audio_codec": "aac",
            "audio_bitrate": 128_000,
        }),
    ))?;

    runtime.connect(
        output::<CameraProcessor::OutputLink::video>(&camera),
        input::<Mp4WriterProcessor::InputLink::video>(&mp4_writer),
    )?;
    runtime.connect(
        output::<AudioCaptureProcessor::OutputLink::audio>(&audio_capture),
        input::<AudioResamplerProcessor::InputLink::audio_in>(&resampler),
    )?;
    runtime.connect(
        output::<AudioResamplerProcessor::OutputLink::audio_out>(&resampler),
        input::<AudioChannelConverterProcessor::InputLink::audio_in>(&channel_converter),
    )?;
    runtime.connect(
        output::<AudioChannelConverterProcessor::OutputLink::audio_out>(&channel_converter),
        input::<Mp4WriterProcessor::InputLink::audio>(&mp4_writer),
    )?;

    println!("▶️  Starting recording pipeline...");
    println!("   Recording to: {}", output_path);
    println!("   Press Ctrl+C to stop recording\n");

    runtime.start()?;
    runtime.wait_for_signal()?;

    println!("\n✅ Recording stopped");
    println!("✅ MP4 file finalized: {}", output_path);

    Ok(())
}
