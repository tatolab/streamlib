// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! MoQ A/V Subscriber — receives, decodes, and plays H.264 video + Opus audio from a MoQ relay.
//!
//! Uses MoqDecodeSubscribeProcessor to subscribe to "video" and "audio" tracks,
//! decode them (H.264 via FFmpeg/VideoToolbox, Opus via libopus), and output
//! decoded frames to DisplayProcessor (window) and AudioOutputProcessor (speakers).

use streamlib::{
    input, output, AudioOutputProcessor, DisplayProcessor, MoqDecodeSubscribeProcessor, Result,
    StreamRuntime,
};

const DEFAULT_RELAY_URL: &str = "https://draft-14.cloudflare.mediaoverquic.com";
const DEFAULT_BROADCAST_PATH: &str = "streamlib-test";

fn main() -> Result<()> {
    // Install rustls crypto provider for QUIC/TLS (MoQ transport)
    let _ = rustls::crypto::ring::default_provider().install_default();

    let relay_url =
        std::env::var("RELAY_URL").unwrap_or_else(|_| DEFAULT_RELAY_URL.to_string());
    let broadcast_path =
        std::env::var("BROADCAST_PATH").unwrap_or_else(|_| DEFAULT_BROADCAST_PATH.to_string());

    tracing::info!("=== MoQ A/V Player ===");
    tracing::info!("Relay:     {}", relay_url);
    tracing::info!("Broadcast: {}", broadcast_path);

    let runtime = StreamRuntime::new()?;

    // MoQ decode subscriber — connects to relay, subscribes to video+audio tracks,
    // decodes H.264 and Opus, outputs Videoframe + Audioframe
    let moq_config = streamlib::_generated_::MoqDecodeSubscribeConfig {
        relay_endpoint_url: relay_url,
        broadcast_path,
        tls_disable_verify: std::env::var("TLS_DISABLE_VERIFY")
            .map(|v| v == "1" || v == "true")
            .ok(),
        audio_sample_rate: Some(48000),
        audio_channels: Some(2),
    };
    let moq_subscriber =
        runtime.add_processor(MoqDecodeSubscribeProcessor::Processor::node(moq_config))?;

    // Display processor — opens a window and renders decoded video frames
    let display_config = DisplayProcessor::Config {
        width: 1920,
        height: 1080,
        title: Some("MoQ A/V Player".to_string()),
        ..Default::default()
    };
    let display = runtime.add_processor(DisplayProcessor::node(display_config))?;

    // Audio output processor — plays decoded audio through speakers via cpal
    let audio_output =
        runtime.add_processor(AudioOutputProcessor::Processor::node(Default::default()))?;

    // Connect MoQ subscriber outputs → display and audio processors
    runtime.connect(
        output::<MoqDecodeSubscribeProcessor::OutputLink::video_out>(&moq_subscriber),
        input::<DisplayProcessor::InputLink::video>(&display),
    )?;

    runtime.connect(
        output::<MoqDecodeSubscribeProcessor::OutputLink::audio_out>(&moq_subscriber),
        input::<AudioOutputProcessor::InputLink::audio>(&audio_output),
    )?;

    tracing::info!("Pipeline: MoQ → decode → display + audio");
    tracing::info!("Press Ctrl+C to stop.");

    runtime.start()?;
    runtime.wait_for_signal()?;

    tracing::info!("MoQ A/V player stopped.");

    Ok(())
}
