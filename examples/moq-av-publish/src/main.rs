// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

// MoQ A/V Publisher
//
// Captures camera + microphone, encodes H.264 video + Opus audio,
// and publishes both tracks to a MoQ relay via MoqPublishProcessor.

use streamlib::_generated_::com_streamlib_moq_publish_config::{Audio, Relay, Video};
use streamlib::_generated_::com_tatolab_audio_channel_converter_config::Mode;
use streamlib::_generated_::com_tatolab_audio_resampler_config::Quality;
use streamlib::{
    input, output, request_audio_permission, request_camera_permission, AudioCaptureProcessor,
    AudioChannelConverterProcessor, AudioResamplerProcessor, BufferRechunkerProcessor,
    CameraProcessor, MoqPublishProcessor, Result, StreamRuntime,
};

use std::time::{SystemTime, UNIX_EPOCH};

const DEFAULT_RELAY_URL: &str = "https://draft-14.cloudflare.mediaoverquic.com";

fn generate_broadcast_path() -> String {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis();
    format!("streamlib-av-{ts}")
}

fn main() -> Result<()> {
    tracing_subscriber::fmt::init();
    let _ = rustls::crypto::ring::default_provider().install_default();

    let relay_url = std::env::var("RELAY_URL").unwrap_or_else(|_| DEFAULT_RELAY_URL.to_string());
    let broadcast_path =
        std::env::var("BROADCAST_PATH").unwrap_or_else(|_| generate_broadcast_path());
    let tls_disable_verify = std::env::var("TLS_DISABLE_VERIFY")
        .map(|v| v == "1" || v == "true")
        .unwrap_or(false);

    println!("=== MoQ A/V Publisher (Real Capture + Encoding) ===\n");

    let runtime = StreamRuntime::new()?;

    // Request permissions (macOS/iOS prompts; no-op on Linux)
    println!("Requesting camera permission...");
    if !request_camera_permission()? {
        eprintln!("Camera permission denied!");
        return Ok(());
    }
    println!("Camera permission granted\n");

    println!("Requesting microphone permission...");
    if !request_audio_permission()? {
        eprintln!("Microphone permission denied!");
        return Ok(());
    }
    println!("Microphone permission granted\n");

    // Camera capture
    println!("Adding camera processor...");
    let camera = runtime.add_processor(CameraProcessor::node(CameraProcessor::Config {
        device_id: None,
        ..Default::default()
    }))?;
    println!("Camera added (1280x720 @ 30fps)\n");

    // Audio pipeline: capture → resample → stereo → rechunk (required for Opus)
    println!("Adding audio capture processor...");
    let audio_pipeline = match runtime.add_processor(AudioCaptureProcessor::node(
        AudioCaptureProcessor::Config {
            device_id: None,
        },
    )) {
        Ok(audio_capture) => {
            println!("Audio capture added (mono @ 24kHz)\n");

            let resampler = runtime.add_processor(AudioResamplerProcessor::node(
                AudioResamplerProcessor::Config {
                    source_sample_rate: 24000,
                    target_sample_rate: 48000,
                    quality: Quality::High,
                },
            ))?;

            let channel_converter = runtime.add_processor(AudioChannelConverterProcessor::node(
                AudioChannelConverterProcessor::Config {
                    mode: Mode::Duplicate,
                    output_channels: None,
                },
            ))?;

            let rechunker = runtime.add_processor(BufferRechunkerProcessor::node(
                BufferRechunkerProcessor::Config {
                    target_buffer_size: 960,
                },
            ))?;

            Some((audio_capture, resampler, channel_converter, rechunker))
        }
        Err(e) => {
            println!("Audio capture unavailable ({}), publishing video only\n", e);
            None
        }
    };

    // MoQ publish processor (encodes + publishes)
    println!("Adding MoQ publish processor...");
    let moq_publish = runtime.add_processor(MoqPublishProcessor::node(
        MoqPublishProcessor::Config {
            relay: Relay {
                endpoint_url: relay_url.clone(),
                broadcast_path: broadcast_path.clone(),
                tls_disable_verify: if tls_disable_verify {
                    Some(true)
                } else {
                    None
                },
            },
            video: Video {
                width: 1280,
                height: 720,
                fps: 30,
                bitrate_bps: 2_500_000, // 2.5 Mbps
            },
            audio: Audio {
                sample_rate: 48000,
                channels: 2,          // Stereo
                bitrate_bps: 128_000, // 128 kbps
            },
        },
    ))?;
    println!("MoQ publish processor added\n");

    // Connect pipeline
    println!("Connecting pipeline:");

    println!("  camera.video -> moq_publish.video_in");
    runtime.connect(
        output::<CameraProcessor::OutputLink::video>(&camera),
        input::<MoqPublishProcessor::InputLink::video_in>(&moq_publish),
    )?;
    println!("  Camera -> MoQ publish");

    if let Some((audio_capture, resampler, channel_converter, rechunker)) = &audio_pipeline {
        println!("  audio_capture.audio -> resampler.audio_in");
        runtime.connect(
            output::<AudioCaptureProcessor::OutputLink::audio>(audio_capture),
            input::<AudioResamplerProcessor::InputLink::audio_in>(resampler),
        )?;
        println!("  Audio capture -> Resampler");

        println!("  resampler.audio_out -> channel_converter.audio_in");
        runtime.connect(
            output::<AudioResamplerProcessor::OutputLink::audio_out>(resampler),
            input::<AudioChannelConverterProcessor::InputLink::audio_in>(channel_converter),
        )?;
        println!("  Resampler -> Channel converter");

        println!("  channel_converter.audio_out -> rechunker.audio_in");
        runtime.connect(
            output::<AudioChannelConverterProcessor::OutputLink::audio_out>(channel_converter),
            input::<BufferRechunkerProcessor::InputLink::audio_in>(rechunker),
        )?;
        println!("  Channel converter -> Rechunker");

        println!("  rechunker.audio_out -> moq_publish.audio_in");
        runtime.connect(
            output::<BufferRechunkerProcessor::OutputLink::audio_out>(rechunker),
            input::<MoqPublishProcessor::InputLink::audio_in>(&moq_publish),
        )?;
        println!("  Rechunker -> MoQ publish\n");
    } else {
        println!("  (audio pipeline skipped - no audio device)\n");
    }

    println!("Pipeline connected\n");

    println!("Starting MoQ A/V publishing...");
    println!("  Relay:     {}", relay_url);
    println!("  Broadcast: {}", broadcast_path);
    println!("  Video: H.264 Baseline 1280x720 @ 30fps, 2.5 Mbps");
    println!("  Audio: Opus 48kHz stereo @ 128 kbps");
    println!("  Tracks: \"video\" (H.264), \"audio\" (Opus)\n");
    println!("Press Ctrl+C to stop publishing\n");

    runtime.start()?;
    runtime.wait_for_signal()?;

    println!("\nPublishing stopped, cleaning up...");
    Ok(())
}
