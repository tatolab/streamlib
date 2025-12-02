use streamlib::core::{
    AudioCaptureConfig, AudioChannelConverterConfig, AudioResamplerConfig, BufferRechunkerConfig,
    CameraConfig, ChannelConversionMode, ResamplingQuality,
};
use streamlib::{
    input, output, request_audio_permission, request_camera_permission, AudioCaptureProcessor,
    AudioChannelConverterProcessor, AudioEncoderConfig, AudioResamplerProcessor,
    BufferRechunkerProcessor, CameraProcessor, H264Profile, Result, StreamRuntime, VideoCodec,
    VideoEncoderConfig, WebRtcWhipConfig, WebRtcWhipProcessor, WhipConfig,
};

fn main() -> Result<()> {
    // Initialize rustls crypto provider (required by webrtc crate)
    // MUST be called before any TLS/DTLS operations
    let _ = rustls::crypto::ring::default_provider().install_default();

    // Initialize tracing
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    println!("=== WebRTC WHIP Streaming to Cloudflare Stream ===\n");

    // Create runtime first
    let mut runtime = StreamRuntime::new();

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
    let camera = runtime.add_processor::<CameraProcessor::Processor>(CameraConfig {
        device_id: None, // Use default camera
    })?;
    println!("✓ Camera added (capturing video @ 1280x720)\n");

    println!("🎤 Adding audio capture processor...");
    let audio_capture =
        runtime.add_processor::<AudioCaptureProcessor::Processor>(AudioCaptureConfig {
            device_id: None, // Use default microphone
        })?;
    println!("✓ Audio capture added (mono @ 24kHz)\n");

    println!("🔄 Adding audio resampler (24kHz → 48kHz)...");
    let resampler =
        runtime.add_processor::<AudioResamplerProcessor::Processor>(AudioResamplerConfig {
            source_sample_rate: 24000,
            target_sample_rate: 48000,
            quality: ResamplingQuality::High,
        })?;
    println!("✓ Resampler added\n");

    println!("🎛️  Adding channel converter (mono → stereo)...");
    let channel_converter = runtime.add_processor::<AudioChannelConverterProcessor::Processor>(
        AudioChannelConverterConfig {
            mode: ChannelConversionMode::Duplicate,
        },
    )?;
    println!("✓ Channel converter added\n");

    println!("📦 Adding buffer rechunker (512 samples → 960 samples for Opus)...");
    let rechunker =
        runtime.add_processor::<BufferRechunkerProcessor::Processor>(BufferRechunkerConfig {
            target_buffer_size: 960, // 20ms @ 48kHz (Opus requirement)
        })?;
    println!("✓ Buffer rechunker added\n");

    println!("🌐 Adding WebRTC WHIP streaming processor...");
    let webrtc = runtime.add_processor::<WebRtcWhipProcessor::Processor>(WebRtcWhipConfig {
        whip: WhipConfig {
            endpoint_url: whip_url.to_string(),
            auth_token: None, // Cloudflare endpoint doesn't require authentication
            timeout_ms: 10000,
        },
        video: VideoEncoderConfig {
            width: 1280,
            height: 720,
            fps: 30,
            bitrate_bps: 2_500_000,                         // 2.5 Mbps
            keyframe_interval_frames: 30, // Every 1 second @ 30fps (for mid-stream join support)
            codec: VideoCodec::H264(H264Profile::Baseline), // Cloudflare requires Baseline
            low_latency: true,
        },
        audio: AudioEncoderConfig {
            sample_rate: 48000,
            channels: 2,           // Stereo
            bitrate_bps: 128_000,  // 128 kbps
            frame_duration_ms: 20, // 20ms frames (WebRTC standard)
            complexity: 10,        // Maximum quality
            vbr: false,            // Constant bitrate for consistent streaming
        },
    })?;
    println!("✓ WebRTC WHIP processor added\n");

    println!("🔗 Connecting pipeline:");

    println!("   camera.video → webrtc.video_in");
    runtime.connect(
        output::<CameraProcessor::OutputLink::video>(&camera),
        input::<WebRtcWhipProcessor::InputLink::video_in>(&webrtc),
    )?;
    println!("   ✓ Camera → WebRTC");

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

    println!("   channel_converter.audio_out → rechunker.audio_in");
    runtime.connect(
        output::<AudioChannelConverterProcessor::OutputLink::audio_out>(&channel_converter),
        input::<BufferRechunkerProcessor::InputLink::audio_in>(&rechunker),
    )?;
    println!("   ✓ Channel converter → rechunker");

    println!("   rechunker.audio_out → webrtc.audio_in");
    runtime.connect(
        output::<BufferRechunkerProcessor::OutputLink::audio_out>(&rechunker),
        input::<WebRtcWhipProcessor::InputLink::audio_in>(&webrtc),
    )?;
    println!("   ✓ Rechunker → WebRTC\n");

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
