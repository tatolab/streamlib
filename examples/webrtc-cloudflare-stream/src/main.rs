// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

// Import nested config types from generated modules
use streamlib::_generated_::com_streamlib_webrtc_whip_config::{Audio, Video, Whip};
use streamlib::_generated_::com_tatolab_audio_channel_converter_config::Mode;
use streamlib::_generated_::com_tatolab_audio_resampler_config::Quality;
use streamlib::{
    input, output, request_audio_permission, request_camera_permission, AudioCaptureProcessor,
    AudioChannelConverterProcessor, AudioResamplerProcessor, BufferRechunkerProcessor,
    CameraProcessor, Result, StreamRuntime, WebRtcWhipProcessor,
};

fn main() -> Result<()> {
    // Initialize rustls crypto provider (required by webrtc crate)
    // MUST be called before any TLS/DTLS operations
    let _ = rustls::crypto::ring::default_provider().install_default();

    println!("=== WebRTC WHIP Streaming to Cloudflare Stream ===\n");

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

    // Cloudflare Stream WHIP endpoint
    let whip_url = "https://customer-5xiy6nkciicmt85v.cloudflarestream.com/4e48912c1e10e84c9bab3777695145dbk0072e99f6ddb152545830a794d165fce/webRTC/publish";

    println!("📹 Adding camera processor...");
    let camera = runtime.add_processor(CameraProcessor::node(CameraProcessor::Config {
        device_id: None, // Use default camera
        ..Default::default()
    }))?;
    println!("✓ Camera added (capturing video @ 1280x720)\n");

    // Audio pipeline is optional — skip if no audio device is available
    println!("🎤 Adding audio capture processor...");
    let audio_pipeline = match runtime.add_processor(AudioCaptureProcessor::node(
        AudioCaptureProcessor::Config {
            device_id: None, // Use default microphone
        },
    )) {
        Ok(audio_capture) => {
            println!("✓ Audio capture added (mono @ 24kHz)\n");

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
            println!("⚠️  Audio capture unavailable ({}), streaming video only\n", e);
            None
        }
    };

    println!("🌐 Adding WebRTC WHIP streaming processor...");
    let webrtc = runtime.add_processor(WebRtcWhipProcessor::node(WebRtcWhipProcessor::Config {
        whip: Whip {
            endpoint_url: whip_url.to_string(),
            auth_token: None, // Cloudflare endpoint doesn't require authentication
            timeout_ms: 10000,
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
    }))?;
    println!("✓ WebRTC WHIP processor added\n");

    println!("🔗 Connecting pipeline:");

    println!("   camera.video → webrtc.video_in");
    runtime.connect(
        output::<CameraProcessor::OutputLink::video>(&camera),
        input::<WebRtcWhipProcessor::InputLink::video_in>(&webrtc),
    )?;
    println!("   ✓ Camera → WebRTC");

    if let Some((audio_capture, resampler, channel_converter, rechunker)) = &audio_pipeline {
        println!("   audio_capture.audio → resampler.audio_in");
        runtime.connect(
            output::<AudioCaptureProcessor::OutputLink::audio>(audio_capture),
            input::<AudioResamplerProcessor::InputLink::audio_in>(resampler),
        )?;
        println!("   ✓ Audio capture → resampler");

        println!("   resampler.audio_out → channel_converter.audio_in");
        runtime.connect(
            output::<AudioResamplerProcessor::OutputLink::audio_out>(resampler),
            input::<AudioChannelConverterProcessor::InputLink::audio_in>(channel_converter),
        )?;
        println!("   ✓ Resampler → channel converter");

        println!("   channel_converter.audio_out → rechunker.audio_in");
        runtime.connect(
            output::<AudioChannelConverterProcessor::OutputLink::audio_out>(channel_converter),
            input::<BufferRechunkerProcessor::InputLink::audio_in>(rechunker),
        )?;
        println!("   ✓ Channel converter → rechunker");

        println!("   rechunker.audio_out → webrtc.audio_in");
        runtime.connect(
            output::<BufferRechunkerProcessor::OutputLink::audio_out>(rechunker),
            input::<WebRtcWhipProcessor::InputLink::audio_in>(&webrtc),
        )?;
        println!("   ✓ Rechunker → WebRTC\n");
    } else {
        println!("   (audio pipeline skipped — no audio device)\n");
    }

    println!("✅ Pipeline connected\n");

    println!("🚀 Starting WebRTC streaming to Cloudflare...");
    println!("   WHIP endpoint: {}", whip_url);
    println!("   Video: H.264 Baseline 1280x720 @ 30fps, 2.5 Mbps");
    println!("   Audio: Opus 48kHz stereo @ 128 kbps\n");
    println!("📺 View your stream at:");
    println!("   https://customer-5xiy6nkciicmt85v.cloudflarestream.com/4e48912c1e10e84c9bab3777695145dbk0072e99f6ddb152545830a794d165fce\n");
    println!("⏹️  Press Ctrl+C to stop streaming\n");

    // start() blocks on macOS standalone (runs NSApplication event loop)
    runtime.start()?;

    runtime.wait_for_signal()?;

    println!("\n✅ Streaming stopped, cleaning up...");
    Ok(())
}
