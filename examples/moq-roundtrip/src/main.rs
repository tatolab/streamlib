// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! MoQ Roundtrip Example
//!
//! Demonstrates MoQ publish/subscribe track processors wired into a StreamLib graph.
//! Publishes multiple tracks (audio + simulated sensor data) through a MoQ relay
//! and subscribes to the same tracks, proving type-agnostic byte-forwarding works
//! end-to-end with multiple data types.
//!
//! All MoQ config is automatic:
//! - Relay: Cloudflare draft-14 (built-in)
//! - Broadcast namespace: auto-generated from runtime ID
//! - Track names: specified per processor
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
    tracing::info!("Publishing: audio + sensor data (simulated)");
    tracing::info!("Subscribing: audio + sensor data");

    let runtime = StreamRuntime::new()?;

    // =========================================================================
    // PUBLISH SIDE
    // =========================================================================

    // Audio source: ChordGenerator produces AudioFrame data
    let audio_source = runtime.add_processor(ChordGeneratorProcessor::Processor::node(
        ChordGeneratorConfig {
            amplitude: 0.3,
            buffer_size: 480,
            sample_rate: 48000,
        },
    ))?;

    // Sensor data source: second ChordGenerator simulating sensor telemetry
    // (In production this would be a real sensor processor producing SensorData frames.
    // Any data type works — MoqPublishTrack forwards raw bytes regardless of type.)
    let sensor_source = runtime.add_processor(ChordGeneratorProcessor::Processor::node(
        ChordGeneratorConfig {
            amplitude: 0.1,
            buffer_size: 128,
            sample_rate: 8000,
        },
    ))?;

    // Video: In a full pipeline you'd wire Camera → H264Encoder → MoqPublishTrack.
    // Omitted here because camera/encoder require hardware. The pattern is identical:
    //   let camera = runtime.add_processor(CameraProcessor::Processor::node(cam_config))?;
    //   let encoder = runtime.add_processor(H264EncoderProcessor::Processor::node(enc_config))?;
    //   runtime.connect(output::<CameraProcessor::OutputLink::video>(&camera),
    //                   input::<H264EncoderProcessor::InputLink::video_in>(&encoder))?;
    //   runtime.connect(output::<H264EncoderProcessor::OutputLink::encoded_out>(&encoder),
    //                   input::<MoqPublishTrackProcessor::InputLink::data_in>(&video_pub))?;

    // MoQ publish tracks — one per data stream
    let audio_pub = runtime.add_processor(MoqPublishTrackProcessor::Processor::node(
        MoqPublishTrackConfig {
            track_name: Some("audio".to_string()),
        },
    ))?;

    let sensor_pub = runtime.add_processor(MoqPublishTrackProcessor::Processor::node(
        MoqPublishTrackConfig {
            track_name: Some("sensor".to_string()),
        },
    ))?;

    // Wire sources → publish tracks
    runtime.connect(
        output::<ChordGeneratorProcessor::OutputLink::chord>(&audio_source),
        input::<MoqPublishTrackProcessor::InputLink::data_in>(&audio_pub),
    )?;

    runtime.connect(
        output::<ChordGeneratorProcessor::OutputLink::chord>(&sensor_source),
        input::<MoqPublishTrackProcessor::InputLink::data_in>(&sensor_pub),
    )?;

    tracing::info!("Publish side wired: audio + sensor → MoQ relay");

    // =========================================================================
    // SUBSCRIBE SIDE
    // =========================================================================

    // MoQ subscribe tracks — one per data stream
    let _audio_sub = runtime.add_processor(MoqSubscribeTrackProcessor::Processor::node(
        MoqSubscribeTrackConfig {
            track_name: "audio".to_string(),
        },
    ))?;

    let _sensor_sub = runtime.add_processor(MoqSubscribeTrackProcessor::Processor::node(
        MoqSubscribeTrackConfig {
            track_name: "sensor".to_string(),
        },
    ))?;

    // Subscribe outputs are unconnected — they log received frames.
    // In a real pipeline you'd wire them to consumers:
    //   runtime.connect(output::<MoqSubscribeTrackProcessor::OutputLink::data_out>(&audio_sub),
    //                   input::<AudioOutputProcessor::InputLink::audio>(&speaker))?;

    tracing::info!("Subscribe side wired: MoQ relay → audio + sensor");

    // =========================================================================
    // RUN
    // =========================================================================

    tracing::info!("Starting MoQ roundtrip pipeline...");
    tracing::info!("Press Ctrl+C to stop.");

    runtime.start()?;
    runtime.wait_for_signal()?;

    tracing::info!("MoQ roundtrip stopped");
    Ok(())
}
