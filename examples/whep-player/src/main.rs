//! WHEP Player using StreamLib's WebRtcWhepProcessor
//!
//! This example demonstrates receiving H.264 video and Opus audio from a WHEP endpoint
//! using StreamLib's integrated WHEP processor with VideoToolbox hardware decoding.

use streamlib::{Result, StreamRuntime};

#[cfg(target_os = "macos")]
use streamlib::{AudioOutputProcessor, DisplayProcessor};

#[cfg(target_os = "macos")]
use streamlib::core::{AudioFrame, DisplayConfig, VideoFrame};

fn main() -> Result<()> {
    // Initialize logging
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    tracing::info!("=== WHEP Player - StreamLib Edition ===\n");

    #[cfg(not(target_os = "macos"))]
    {
        tracing::error!("This example currently only supports macOS");
        return Ok(());
    }

    #[cfg(target_os = "macos")]
    {
        run_whep_player()
    }
}

#[cfg(target_os = "macos")]
fn run_whep_player() -> Result<()> {
    use streamlib::{WebRtcWhepConfig, WebRtcWhepProcessor, WhepConfig};

    // Get WHEP endpoint URL from environment or use Cloudflare Stream default
    let whep_url = std::env::var("WHEP_URL").unwrap_or_else(|_| {
        "https://customer-5xiy6nkciicmt85v.cloudflarestream.com/0072e99f6ddb152545830a794d165fce/webRTC/play".to_string()
    });

    tracing::info!("📡 Connecting to WHEP endpoint:");
    tracing::info!("   {}\n", whep_url);

    // Create StreamRuntime
    let mut runtime = StreamRuntime::new();

    // Configure WHEP processor
    let whep_config = WebRtcWhepConfig {
        whep: WhepConfig {
            endpoint_url: whep_url.clone(),
            auth_token: None,
            timeout_ms: 10000,
        },
    };

    // Create WHEP processor
    tracing::info!("🎬 Creating WHEP processor...");
    let whep_processor = runtime.add_processor_with_config::<WebRtcWhepProcessor>(whep_config)?;
    tracing::info!("✅ WHEP processor created\n");

    // Create display processor for video output
    tracing::info!("📺 Creating display processor...");
    let display = runtime.add_processor_with_config::<DisplayProcessor>(DisplayConfig {
        width: 1920,
        height: 1080,
        title: Some("WHEP Player".to_string()),
        scaling_mode: Default::default(), // Use default scaling (Stretch)
    })?;
    tracing::info!("✅ Display processor created\n");

    // Create audio output processor
    tracing::info!("🔊 Creating audio output processor...");
    let audio_output =
        runtime.add_processor_with_config::<AudioOutputProcessor>(Default::default())?;
    tracing::info!("✅ Audio output processor created\n");

    // Connect processors
    tracing::info!("🔗 Connecting processors...");
    runtime.connect(
        whep_processor.output_port::<VideoFrame>("video_out"),
        display.input_port::<VideoFrame>("video"),
    )?;

    runtime.connect(
        whep_processor.output_port::<AudioFrame>("audio_out"),
        audio_output.input_port::<AudioFrame>("audio"),
    )?;
    tracing::info!("✅ Processors connected\n");

    tracing::info!("🚀 Starting WHEP playback pipeline...\n");

    // Start the runtime
    runtime.start()?;

    tracing::info!("📺 WHEP stream is now playing!");
    tracing::info!("Press Cmd+Q to stop.\n");

    // Run until stopped
    runtime.run()?;

    tracing::info!("✅ WHEP player stopped");

    Ok(())
}
