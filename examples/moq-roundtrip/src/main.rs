// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! MoQ Roundtrip Example
//!
//! Demonstrates MoQ publish/subscribe track processors wired into a StreamLib graph.
//! Publishes audio from a ChordGenerator through a MoQ relay, subscribes to
//! the same track, proving type-agnostic byte-forwarding works end-to-end.
//!
//! Uses the default Cloudflare draft-14 relay with an auto-generated broadcast path.
//!
//! Usage:
//!   cargo run -p moq-roundtrip

use streamlib::{
    input, output,
    ChordGeneratorProcessor,
    MoqPublishTrackConfig, MoqPublishTrackProcessor,
    MoqSubscribeTrackConfig, MoqSubscribeTrackProcessor,
    Result, StreamRuntime,
};
use streamlib::_generated_::ChordGeneratorConfig;

fn main() -> Result<()> {
    tracing_subscriber::fmt::init();
    rustls::crypto::ring::default_provider()
        .install_default()
        .ok();

    tracing::info!("=== MoQ Roundtrip - StreamLib Edition ===");

    // Create StreamRuntime
    let runtime = StreamRuntime::new()?;

    // Create ChordGenerator (audio source)
    let chord = runtime.add_processor(ChordGeneratorProcessor::Processor::node(
        ChordGeneratorConfig {
            amplitude: 0.3,
            buffer_size: 480,
            sample_rate: 48000,
        },
    ))?;
    tracing::info!("Created ChordGenerator");

    // Create MoQ publish track — publishes audio bytes to the relay
    // No URL needed: defaults to Cloudflare draft-14 relay with auto-generated broadcast path
    let moq_pub = runtime.add_processor(MoqPublishTrackProcessor::Processor::node(
        MoqPublishTrackConfig {
            track_name: Some("audio".to_string()),
            ..Default::default()
        },
    ))?;
    tracing::info!("Created MoqPublishTrack");

    // Create MoQ subscribe track — receives audio bytes from the relay
    // Same default relay, just specify which track to subscribe to
    let _moq_sub = runtime.add_processor(MoqSubscribeTrackProcessor::Processor::node(
        MoqSubscribeTrackConfig {
            track_name: "audio".to_string(),
            ..Default::default()
        },
    ))?;
    tracing::info!("Created MoqSubscribeTrack");

    // Wire: ChordGenerator → MoqPublishTrack (any type flows through)
    runtime.connect(
        output::<ChordGeneratorProcessor::OutputLink::chord>(&chord),
        input::<MoqPublishTrackProcessor::InputLink::data_in>(&moq_pub),
    )?;
    tracing::info!("Connected ChordGenerator → MoqPublishTrack");

    // MoqSubscribeTrack output is unconnected — it logs received frames.
    // In a real pipeline you'd wire it to an audio output processor.

    tracing::info!("Starting MoQ roundtrip pipeline...");
    tracing::info!("Press Ctrl+C to stop.");

    runtime.start()?;
    runtime.wait_for_signal()?;

    tracing::info!("MoQ roundtrip stopped");
    Ok(())
}
