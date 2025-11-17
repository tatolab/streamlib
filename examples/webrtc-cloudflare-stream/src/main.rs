use streamlib::{Result, StreamRuntime};
use streamlib::{
    CameraProcessor, AudioCaptureProcessor, WebRtcWhipProcessor,
    AudioResamplerProcessor, AudioChannelConverterProcessor,
    WebRtcWhipConfig, WhipConfig, VideoEncoderConfig, AudioEncoderConfig, H264Profile,
};
use streamlib::core::{
    CameraConfig, AudioCaptureConfig,
    AudioResamplerConfig, ResamplingQuality,
    AudioChannelConverterConfig, ChannelConversionMode,
    VideoFrame, AudioFrame,
};

fn main() -> Result<()> {
    // Initialize tracing
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::DEBUG)
        .init();

    println!("=== WebRTC WHIP Streaming to Cloudflare Stream ===\n");

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

    // Cloudflare Stream WHIP endpoint
    let whip_url = "https://customer-5xiy6nkciicmt85v.cloudflarestream.com/4e48912c1e10e84c9bab3777695145dbk0072e99f6ddb152545830a794d165fce/webRTC/publish";

    println!("📹 Adding camera processor...");
    let camera = runtime.add_processor_with_config::<CameraProcessor>(
        CameraConfig {
            device_id: None, // Use default camera
        }
    )?;
    println!("✓ Camera added (capturing video @ 1280x720)\n");

    println!("🎤 Adding audio capture processor...");
    let audio_capture = runtime.add_processor_with_config::<AudioCaptureProcessor>(
        AudioCaptureConfig {
            device_id: None, // Use default microphone
        }
    )?;
    println!("✓ Audio capture added (mono @ 24kHz)\n");

    println!("🔄 Adding audio resampler (24kHz → 48kHz)...");
    let resampler = runtime.add_processor_with_config::<AudioResamplerProcessor>(
        AudioResamplerConfig {
            source_sample_rate: 24000,
            target_sample_rate: 48000,
            quality: ResamplingQuality::High,
        }
    )?;
    println!("✓ Resampler added\n");

    println!("🎛️  Adding channel converter (mono → stereo)...");
    let channel_converter = runtime.add_processor_with_config::<AudioChannelConverterProcessor>(
        AudioChannelConverterConfig {
            mode: ChannelConversionMode::Duplicate,
        }
    )?;
    println!("✓ Channel converter added\n");

    println!("🌐 Adding WebRTC WHIP streaming processor...");
    let webrtc = runtime.add_processor_with_config::<WebRtcWhipProcessor>(
        WebRtcWhipConfig {
            whip: WhipConfig {
                endpoint_url: whip_url.to_string(),
                auth_token: None, // Cloudflare endpoint doesn't require authentication
                timeout_ms: 10000,
            },
            video: VideoEncoderConfig {
                width: 1280,
                height: 720,
                fps: 30,
                bitrate_bps: 2_500_000, // 2.5 Mbps
                keyframe_interval_frames: 60, // Every 2 seconds @ 30fps
                profile: H264Profile::Baseline, // Cloudflare requires Baseline
                low_latency: true,
            },
            audio: AudioEncoderConfig {
                sample_rate: 48000,
                channels: 2, // Stereo
                bitrate_bps: 128_000, // 128 kbps
                frame_duration_ms: 20, // 20ms frames (WebRTC standard)
                complexity: 10, // Maximum quality
                vbr: false, // Constant bitrate for consistent streaming
            },
        }
    )?;
    println!("✓ WebRTC WHIP processor added\n");

    println!("🔗 Connecting pipeline:");
    println!("   camera.video → webrtc.video_in");
    runtime.connect(
        camera.output_port::<VideoFrame>("video"),
        webrtc.input_port::<VideoFrame>("video_in"),
    )?;
    println!("   ✓ Video connected");

    println!("   audio_capture.audio → resampler.audio_in");
    runtime.connect(
        audio_capture.output_port::<AudioFrame<1>>("audio"),
        resampler.input_port::<AudioFrame<1>>("audio_in"),
    )?;
    println!("   ✓ Audio capture → resampler");

    println!("   resampler.audio_out → channel_converter.audio_in");
    runtime.connect(
        resampler.output_port::<AudioFrame<1>>("audio_out"),
        channel_converter.input_port::<AudioFrame<1>>("audio_in"),
    )?;
    println!("   ✓ Resampler → channel converter");

    println!("   channel_converter.audio_out → webrtc.audio_in");
    runtime.connect(
        channel_converter.output_port::<AudioFrame<2>>("audio_out"),
        webrtc.input_port::<AudioFrame<2>>("audio_in"),
    )?;
    println!("   ✓ Channel converter → WebRTC\n");

    println!("✅ Pipeline connected\n");

    println!("🚀 Starting WebRTC streaming to Cloudflare...");
    println!("   WHIP endpoint: {}", whip_url);
    println!("   Video: H.264 Baseline 1280x720 @ 30fps, 2.5 Mbps");
    println!("   Audio: Opus 48kHz stereo @ 128 kbps\n");
    println!("📺 View your stream at:");
    println!("   https://customer-5xiy6nkciicmt85v.cloudflarestream.com/4e48912c1e10e84c9bab3777695145dbk0072e99f6ddb152545830a794d165fce\n");
    println!("⏹️  Press Ctrl+C to stop streaming\n");

    // Run the runtime (blocks until Ctrl+C)
    runtime.run()?;

    println!("\n✅ Streaming stopped, cleaning up...");
    Ok(())
}
