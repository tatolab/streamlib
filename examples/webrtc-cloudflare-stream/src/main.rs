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

#[cfg(target_os = "linux")]
use streamlib::_generated_::com_streamlib_webrtc_whip_config::{Audio, Video, Whip};
#[cfg(target_os = "linux")]
use streamlib::_generated_::com_tatolab_audio_channel_converter_config::Mode;
#[cfg(target_os = "linux")]
use streamlib::_generated_::com_tatolab_audio_resampler_config::Quality;
#[cfg(target_os = "linux")]
use streamlib::{
    input, output, request_audio_permission, request_camera_permission, AudioCaptureProcessor,
    AudioChannelConverterProcessor, AudioResamplerProcessor, BufferRechunkerProcessor,
    CameraProcessor, H264EncoderConfig, H264EncoderProcessor, OpusEncoderConfig,
    OpusEncoderProcessor, Result, StreamRuntime, WebRtcWhipProcessor,
};

#[cfg(target_os = "linux")]
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
        return Ok(());
    }
    println!("✅ Camera permission granted\n");

    println!("🔒 Requesting microphone permission...");
    if !request_audio_permission()? {
        eprintln!("❌ Microphone permission denied!");
        return Ok(());
    }
    println!("✅ Microphone permission granted\n");

    // Cloudflare Stream WHIP endpoint
    let whip_url = "https://customer-5xiy6nkciicmt85v.cloudflarestream.com/4e48912c1e10e84c9bab3777695145dbk0072e99f6ddb152545830a794d165fce/webRTC/publish";

    // Shared video params (camera + encoder must match).
    let video_width: u32 = 1280;
    let video_height: u32 = 720;
    let video_fps: u32 = 30;
    let video_bitrate: u32 = 2_500_000;
    let audio_bitrate: u32 = 128_000;

    println!("📹 Adding camera processor...");
    let camera = runtime.add_processor(CameraProcessor::node(CameraProcessor::Config {
        device_id: None,
        ..Default::default()
    }))?;
    println!(
        "✓ Camera added (capturing video @ {}x{})\n",
        video_width, video_height
    );

    println!("🎬 Adding H.264 encoder...");
    let h264_encoder = runtime.add_processor(H264EncoderProcessor::node(H264EncoderConfig {
        width: Some(video_width),
        height: Some(video_height),
        fps: Some(video_fps),
        bitrate_bps: Some(video_bitrate),
        profile: Some("baseline".to_string()),
        ..Default::default()
    }))?;
    println!("✓ H.264 encoder added (Vulkan Video, baseline @ {} bps)\n", video_bitrate);

    // Audio pipeline is optional — skip if no audio device is available
    println!("🎤 Adding audio capture processor...");
    let audio_pipeline = match runtime.add_processor(AudioCaptureProcessor::node(
        AudioCaptureProcessor::Config {
            device_id: None,
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

            let opus_encoder = runtime.add_processor(OpusEncoderProcessor::node(
                OpusEncoderConfig {
                    bitrate_bps: Some(audio_bitrate),
                },
            ))?;

            Some((audio_capture, resampler, channel_converter, rechunker, opus_encoder))
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
    }))?;
    println!("✓ WebRTC WHIP processor added\n");

    println!("🔗 Connecting pipeline:");

    println!("   camera.video → h264_encoder.video_in");
    runtime.connect(
        output::<CameraProcessor::OutputLink::video>(&camera),
        input::<H264EncoderProcessor::InputLink::video_in>(&h264_encoder),
    )?;
    println!("   ✓ Camera → H.264 encoder");

    println!("   h264_encoder.encoded_video_out → webrtc.encoded_video_in");
    runtime.connect(
        output::<H264EncoderProcessor::OutputLink::encoded_video_out>(&h264_encoder),
        input::<WebRtcWhipProcessor::InputLink::encoded_video_in>(&webrtc),
    )?;
    println!("   ✓ H.264 encoder → WebRTC\n");

    if let Some((audio_capture, resampler, channel_converter, rechunker, opus_encoder)) =
        &audio_pipeline
    {
        runtime.connect(
            output::<AudioCaptureProcessor::OutputLink::audio>(audio_capture),
            input::<AudioResamplerProcessor::InputLink::audio_in>(resampler),
        )?;
        runtime.connect(
            output::<AudioResamplerProcessor::OutputLink::audio_out>(resampler),
            input::<AudioChannelConverterProcessor::InputLink::audio_in>(channel_converter),
        )?;
        runtime.connect(
            output::<AudioChannelConverterProcessor::OutputLink::audio_out>(channel_converter),
            input::<BufferRechunkerProcessor::InputLink::audio_in>(rechunker),
        )?;
        runtime.connect(
            output::<BufferRechunkerProcessor::OutputLink::audio_out>(rechunker),
            input::<OpusEncoderProcessor::InputLink::audio_in>(opus_encoder),
        )?;
        runtime.connect(
            output::<OpusEncoderProcessor::OutputLink::encoded_audio_out>(opus_encoder),
            input::<WebRtcWhipProcessor::InputLink::encoded_audio_in>(&webrtc),
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
