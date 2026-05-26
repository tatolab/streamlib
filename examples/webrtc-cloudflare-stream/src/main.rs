// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

// The WebRTC WHIP processor accepts pre-encoded frames (H.264 video + Opus
// audio). On Linux the Vulkan-video H264EncoderProcessor plus the
// cross-platform OpusEncoderProcessor complete the pipeline. macOS does not
// yet expose a VideoToolbox-backed H264EncoderProcessor; that port is tracked
// as a follow-up to issue #358.
#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!(
        "webrtc-cloudflare-stream currently requires Linux — no \
         H264EncoderProcessor is exposed on macOS yet. Tracked as a follow-up \
         to issue #358."
    );
    std::process::exit(2);
}

#[allow(non_snake_case, unused_imports, dead_code, clippy::all)]
mod _generated_ {
    include!(concat!(env!("OUT_DIR"), "/_generated_shim.rs"));
}

#[cfg(target_os = "linux")]
use streamlib::sdk::permissions::{request_audio_permission, request_camera_permission};
#[cfg(target_os = "linux")]
use streamlib::sdk::error::Result;
#[cfg(target_os = "linux")]
use streamlib::sdk::graph::{InputLinkPortRef, OutputLinkPortRef};
#[cfg(target_os = "linux")]
use streamlib::sdk::module_ident_any_version;
#[cfg(target_os = "linux")]
use streamlib::sdk::runtime::Runner;
#[cfg(target_os = "linux")]
use streamlib::sdk::processors::ProcessorSpec;
#[cfg(target_os = "linux")]
use streamlib::sdk::schema_ident_any_version;

#[cfg(target_os = "linux")]
use crate::_generated_::tatolab__camera::CameraConfig;
#[cfg(target_os = "linux")]
use crate::_generated_::tatolab__h264::H264EncoderConfig;
#[cfg(target_os = "linux")]
use crate::_generated_::tatolab__opus::OpusEncoderConfig;
#[cfg(target_os = "linux")]
use crate::_generated_::tatolab__webrtc::webrtc_whip_config::{Audio, Video, Whip};
#[cfg(target_os = "linux")]
use crate::_generated_::tatolab__webrtc::WebrtcWhipConfig;
#[cfg(target_os = "linux")]
use crate::_generated_::tatolab__audio::audio_channel_converter_config::Mode;
#[cfg(target_os = "linux")]
use crate::_generated_::tatolab__audio::audio_resampler_config::Quality;
#[cfg(target_os = "linux")]
use crate::_generated_::tatolab__audio::{
    AudioCaptureConfig, AudioChannelConverterConfig, AudioResamplerConfig,
    BufferRechunkerConfig,
};

#[cfg(target_os = "linux")]
fn main() -> Result<()> {
    // Initialize rustls crypto provider (required by webrtc crate).
    let _ = rustls::crypto::ring::default_provider().install_default();

    println!("=== WebRTC WHIP Streaming to Cloudflare Stream ===\n");

    let runtime = Runner::new()?;

    runtime.add_module(module_ident_any_version!("tatolab", "audio"))?;
    runtime.add_module(module_ident_any_version!("tatolab", "camera"))?;
    runtime.add_module(module_ident_any_version!("tatolab", "h264"))?;
    runtime.add_module(module_ident_any_version!("tatolab", "opus"))?;
    runtime.add_module(module_ident_any_version!("tatolab", "webrtc"))?;

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

    let whip_url = "https://customer-5xiy6nkciicmt85v.cloudflarestream.com/4e48912c1e10e84c9bab3777695145dbk0072e99f6ddb152545830a794d165fce/webRTC/publish";

    let video_width: u32 = 1280;
    let video_height: u32 = 720;
    let video_fps: u32 = 30;
    let video_bitrate: u32 = 2_500_000;
    let audio_bitrate: u32 = 128_000;

    println!("📹 Adding camera processor...");
    let camera = runtime.add_processor(ProcessorSpec::new(
        schema_ident_any_version!("tatolab", "camera", "Camera")?,
        serde_json::to_value(CameraConfig::default())
            .map_err(|e| streamlib::sdk::error::Error::Configuration(e.to_string()))?,
    ))?;
    println!("✓ Camera added (capturing video @ {}x{})\n", video_width, video_height);

    println!("🎬 Adding H.264 encoder...");
    let h264_encoder = runtime.add_processor(ProcessorSpec::new(
        schema_ident_any_version!("tatolab", "h264", "H264Encoder")?,
        serde_json::to_value(H264EncoderConfig {
            width: Some(video_width),
            height: Some(video_height),
            fps: Some(video_fps),
            bitrate_bps: Some(video_bitrate),
            profile: Some("baseline".to_string()),
            ..Default::default()
        })
        .map_err(|e| streamlib::sdk::error::Error::Configuration(e.to_string()))?,
    ))?;
    println!("✓ H.264 encoder added (Vulkan Video, baseline @ {} bps)\n", video_bitrate);

    // Audio pipeline is optional — skip if no audio device is available.
    println!("🎤 Adding audio capture processor...");
    let audio_pipeline = match runtime.add_processor(ProcessorSpec::new(
        schema_ident_any_version!("tatolab", "audio", "AudioCapture")?,
        serde_json::to_value(AudioCaptureConfig { device_id: None })
            .map_err(|e| streamlib::sdk::error::Error::Configuration(e.to_string()))?,
    )) {
        Ok(audio_capture) => {
            println!("✓ Audio capture added (mono @ 24kHz)\n");

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

            let rechunker = runtime.add_processor(ProcessorSpec::new(
                schema_ident_any_version!("tatolab", "audio", "BufferRechunker")?,
                serde_json::to_value(BufferRechunkerConfig {
                    target_buffer_size: 960,
                })
                .map_err(|e| streamlib::sdk::error::Error::Configuration(e.to_string()))?,
            ))?;

            let opus_encoder = runtime.add_processor(ProcessorSpec::new(
                schema_ident_any_version!("tatolab", "opus", "OpusEncoder")?,
                serde_json::to_value(OpusEncoderConfig {
                    bitrate_bps: Some(audio_bitrate),
                })
                .map_err(|e| streamlib::sdk::error::Error::Configuration(e.to_string()))?,
            ))?;

            Some((audio_capture, resampler, channel_converter, rechunker, opus_encoder))
        }
        Err(e) => {
            println!("⚠️  Audio capture unavailable ({}), streaming video only\n", e);
            None
        }
    };

    println!("🌐 Adding WebRTC WHIP streaming processor...");
    let webrtc = runtime.add_processor(ProcessorSpec::new(
        schema_ident_any_version!("tatolab", "webrtc", "WebrtcWhip")?,
        serde_json::to_value(WebrtcWhipConfig {
            whip: Whip {
                endpoint_url: whip_url.to_string(),
                auth_token: None,
                timeout_ms: 10000,
            },
            video: Video {
                width: video_width,
                height: video_height,
                fps: video_fps,
                bitrate_bps: video_bitrate,
            },
            audio: Audio {
                sample_rate: 48000,
                channels: 2,
                bitrate_bps: audio_bitrate,
            },
        })
        .map_err(|e| streamlib::sdk::error::Error::Configuration(e.to_string()))?,
    ))?;
    println!("✓ WebRTC WHIP processor added\n");

    println!("🔗 Connecting pipeline:");

    runtime.connect(
        OutputLinkPortRef::new(&camera, "video"),
        InputLinkPortRef::new(&h264_encoder, "video_in"),
    )?;
    println!("   ✓ Camera → H.264 encoder");

    runtime.connect(
        OutputLinkPortRef::new(&h264_encoder, "encoded_video_out"),
        InputLinkPortRef::new(&webrtc, "encoded_video_in"),
    )?;
    println!("   ✓ H.264 encoder → WebRTC\n");

    if let Some((audio_capture, resampler, channel_converter, rechunker, opus_encoder)) =
        &audio_pipeline
    {
        runtime.connect(
            OutputLinkPortRef::new(audio_capture, "audio"),
            InputLinkPortRef::new(resampler, "audio_in"),
        )?;
        runtime.connect(
            OutputLinkPortRef::new(resampler, "audio_out"),
            InputLinkPortRef::new(channel_converter, "audio_in"),
        )?;
        runtime.connect(
            OutputLinkPortRef::new(channel_converter, "audio_out"),
            InputLinkPortRef::new(rechunker, "audio_in"),
        )?;
        runtime.connect(
            OutputLinkPortRef::new(rechunker, "audio_out"),
            InputLinkPortRef::new(opus_encoder, "audio_in"),
        )?;
        runtime.connect(
            OutputLinkPortRef::new(opus_encoder, "encoded_audio_out"),
            InputLinkPortRef::new(&webrtc, "encoded_audio_in"),
        )?;
        println!("   ✓ Mic → resampler → converter → rechunker → Opus → WebRTC\n");
    } else {
        println!("   (audio pipeline skipped — no audio device)\n");
    }

    println!("✅ Pipeline connected\n");
    println!("🚀 Starting WebRTC streaming to Cloudflare...");
    println!("   WHIP endpoint: {}", whip_url);
    println!(
        "   Video: H.264 baseline {}x{} @ {}fps, {} bps",
        video_width, video_height, video_fps, video_bitrate
    );
    println!("   Audio: Opus 48kHz stereo @ {} bps\n", audio_bitrate);
    println!("⏹️  Press Ctrl+C to stop streaming\n");

    runtime.start()?;
    runtime.wait_for_signal()?;

    println!("\n✅ Streaming stopped, cleaning up...");
    Ok(())
}
