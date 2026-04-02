// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! MoQ Roundtrip Example
//!
//! Demonstrates MoQ publish/subscribe track processors wired into a StreamLib graph.
//! Publishes audio from a ChordGenerator through a MoQ relay, subscribes to
//! the same track, proving type-agnostic byte-forwarding works end-to-end.
//!
//! Usage:
//!   cargo run -p moq-roundtrip -- <relay_url>/<broadcast_path>
//!
//! Example:
//!   cargo run -p moq-roundtrip -- https://draft-14.cloudflare.mediaoverquic.com/moq-roundtrip-test

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

    let args: Vec<String> = std::env::args().collect();
    let moq_url = if args.len() >= 2 {
        args[1].clone()
    } else {
        eprintln!("Usage: {} <relay_url>/<broadcast_path>", args[0]);
        eprintln!(
            "Example: {} https://draft-14.cloudflare.mediaoverquic.com/moq-roundtrip-test",
            args[0]
        );
        std::process::exit(1);
    };

    tracing::info!("=== MoQ Roundtrip - StreamLib Edition ===");
    tracing::info!("  URL: {}", moq_url);

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
    let moq_pub = runtime.add_processor(MoqPublishTrackProcessor::Processor::node(
        MoqPublishTrackConfig {
            url: moq_url.clone(),
            track_name: Some("audio".to_string()),
        },
    ))?;
    tracing::info!("Created MoqPublishTrack");

    // Create MoQ subscribe track — receives audio bytes from the relay
    let _moq_sub = runtime.add_processor(MoqSubscribeTrackProcessor::Processor::node(
        MoqSubscribeTrackConfig {
            url: moq_url,
            track_name: "audio".to_string(),
        },
    ))?;
    tracing::info!("Created MoqSubscribeTrack");

    // Wire: ChordGenerator → MoqPublishTrack
    runtime.connect(
        output::<ChordGeneratorProcessor::OutputLink::chord>(&chord),
        input::<MoqPublishTrackProcessor::InputLink::data_in>(&moq_pub),
    )?;
    tracing::info!("Connected ChordGenerator → MoqPublishTrack");

    // MoqSubscribeTrack output is unconnected — it logs received frames.
    // In a real pipeline you'd wire it to an audio decoder or output processor.

    tracing::info!("Starting MoQ roundtrip pipeline...");
    tracing::info!("Press Ctrl+C to stop.");

    runtime.start()?;
    runtime.wait_for_signal()?;

    tracing::info!("MoQ roundtrip stopped");
    Ok(())
}
