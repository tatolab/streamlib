// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! MoQ Roundtrip Example
//!
//! Demonstrates the full StreamLib MoQ pipeline with composable processors:
//!
//! PUBLISH SIDE:
//!   Camera → H264Encoder → MoqPublishTrack (video)
//!   AudioCapture → OpusEncoder → MoqPublishTrack (audio)
//!   ChordGenerator → MoqPublishTrack (sensor sim)
//!
//! SUBSCRIBE SIDE:
//!   MoqSubscribeTrack → H264Decoder → Display (video)
//!   MoqSubscribeTrack → OpusDecoder → AudioOutput (audio)
//!   MoqSubscribeTrack (sensor data logged)
//!
//! All MoQ config is automatic — Cloudflare draft-14 relay, auto-generated
//! namespace.
//!
//! There is no module-loading call: every processor's package
//! (`@tatolab/{audio,camera,display,h264,moq,opus}`) lives in this app's
//! `streamlib_modules/` folder (populated by `./setup.sh`), and the runtime
//! lazily discovers + loads each on the first `processor_type_ref!` reference.
//! The reference sites carry no version.

use streamlib::sdk::RunnerAutoBuild;
use streamlib::sdk::error::Result;
use streamlib::sdk::graph::{InputLinkPortRef, OutputLinkPortRef};
use streamlib::sdk::processor_type_ref;
use streamlib::sdk::processors::ProcessorSpec;
use streamlib::sdk::runtime::Runner;

fn main() -> Result<()> {
    rustls::crypto::ring::default_provider()
        .install_default()
        .ok();

    tracing::info!("=== MoQ Roundtrip ===");
    tracing::info!("Publishing: camera (H.264) + audio (Opus) + sensor data");
    tracing::info!("Subscribing: video → display, audio → speaker, sensor → log");

    let runtime = Runner::with_auto_build()?;

    // ---- PUBLISH SIDE ----

    let camera = runtime.add_processor(ProcessorSpec::new(
        processor_type_ref!("tatolab", "camera", "Camera"),
        serde_json::json!({}),
    ))?;
    let h264_enc = runtime.add_processor(ProcessorSpec::new(
        processor_type_ref!("tatolab", "h264", "H264Encoder"),
        serde_json::json!({ "keyframe_interval_seconds": 2.0 }),
    ))?;
    let video_pub = runtime.add_processor(ProcessorSpec::new(
        processor_type_ref!("tatolab", "moq", "MoqPublishTrack"),
        serde_json::json!({ "track_name": "video" }),
    ))?;

    runtime.connect(
        OutputLinkPortRef::new(&camera, "video"),
        InputLinkPortRef::new(&h264_enc, "video_in"),
    )?;
    runtime.connect(
        OutputLinkPortRef::new(&h264_enc, "encoded_video_out"),
        InputLinkPortRef::new(&video_pub, "data_in"),
    )?;

    let mic = runtime.add_processor(ProcessorSpec::new(
        processor_type_ref!("tatolab", "audio", "AudioCapture"),
        serde_json::json!({}),
    ))?;
    let resampler = runtime.add_processor(ProcessorSpec::new(
        processor_type_ref!("tatolab", "audio", "AudioResampler"),
        // `source_sample_rate` is required by the config schema but advisory —
        // the resampler derives the real source rate from each input frame.
        serde_json::json!({
            "source_sample_rate": 48000,
            "target_sample_rate": 48000,
            "quality": "High",
        }),
    ))?;
    let rechunker = runtime.add_processor(ProcessorSpec::new(
        processor_type_ref!("tatolab", "audio", "BufferRechunker"),
        serde_json::json!({ "target_buffer_size": 960 }),
    ))?;
    let opus_enc = runtime.add_processor(ProcessorSpec::new(
        processor_type_ref!("tatolab", "opus", "OpusEncoder"),
        serde_json::json!({}),
    ))?;
    let audio_pub = runtime.add_processor(ProcessorSpec::new(
        processor_type_ref!("tatolab", "moq", "MoqPublishTrack"),
        serde_json::json!({ "track_name": "audio" }),
    ))?;

    runtime.connect(
        OutputLinkPortRef::new(&mic, "audio"),
        InputLinkPortRef::new(&resampler, "audio_in"),
    )?;
    runtime.connect(
        OutputLinkPortRef::new(&resampler, "audio_out"),
        InputLinkPortRef::new(&rechunker, "audio_in"),
    )?;
    runtime.connect(
        OutputLinkPortRef::new(&rechunker, "audio_out"),
        InputLinkPortRef::new(&opus_enc, "audio_in"),
    )?;
    runtime.connect(
        OutputLinkPortRef::new(&opus_enc, "encoded_audio_out"),
        InputLinkPortRef::new(&audio_pub, "data_in"),
    )?;

    let sensor = runtime.add_processor(ProcessorSpec::new(
        processor_type_ref!("tatolab", "audio", "ChordGenerator"),
        serde_json::json!({
            "amplitude": 0.1,
            "buffer_size": 128,
            "sample_rate": 8000,
        }),
    ))?;
    let sensor_pub = runtime.add_processor(ProcessorSpec::new(
        processor_type_ref!("tatolab", "moq", "MoqPublishTrack"),
        serde_json::json!({ "track_name": "sensor" }),
    ))?;

    runtime.connect(
        OutputLinkPortRef::new(&sensor, "chord"),
        InputLinkPortRef::new(&sensor_pub, "data_in"),
    )?;

    tracing::info!("Publish side wired: camera + audio + sensor → MoQ relay");

    // ---- SUBSCRIBE SIDE ----

    let video_sub = runtime.add_processor(ProcessorSpec::new(
        processor_type_ref!("tatolab", "moq", "MoqSubscribeTrack"),
        serde_json::json!({ "track_name": "video" }),
    ))?;
    let h264_dec = runtime.add_processor(ProcessorSpec::new(
        processor_type_ref!("tatolab", "h264", "H264Decoder"),
        serde_json::json!({}),
    ))?;
    let display = runtime.add_processor(ProcessorSpec::new(
        processor_type_ref!("tatolab", "display", "Display"),
        serde_json::json!({
            "width": 1280,
            "height": 720,
            "title": "MoQ Roundtrip",
        }),
    ))?;

    runtime.connect(
        OutputLinkPortRef::new(&video_sub, "data_out"),
        InputLinkPortRef::new(&h264_dec, "encoded_video_in"),
    )?;
    runtime.connect(
        OutputLinkPortRef::new(&h264_dec, "video_out"),
        InputLinkPortRef::new(&display, "video"),
    )?;

    let audio_sub = runtime.add_processor(ProcessorSpec::new(
        processor_type_ref!("tatolab", "moq", "MoqSubscribeTrack"),
        serde_json::json!({ "track_name": "audio" }),
    ))?;
    let opus_dec = runtime.add_processor(ProcessorSpec::new(
        processor_type_ref!("tatolab", "opus", "OpusDecoder"),
        serde_json::json!({}),
    ))?;
    let sub_resampler = runtime.add_processor(ProcessorSpec::new(
        processor_type_ref!("tatolab", "audio", "AudioResampler"),
        serde_json::json!({
            "source_sample_rate": 48000,
            "target_sample_rate": 44100,
            "quality": "High",
        }),
    ))?;
    let sub_rechunker = runtime.add_processor(ProcessorSpec::new(
        processor_type_ref!("tatolab", "audio", "BufferRechunker"),
        serde_json::json!({ "target_buffer_size": 512 }),
    ))?;
    let speaker = runtime.add_processor(ProcessorSpec::new(
        processor_type_ref!("tatolab", "audio", "AudioOutput"),
        serde_json::json!({}),
    ))?;

    runtime.connect(
        OutputLinkPortRef::new(&audio_sub, "data_out"),
        InputLinkPortRef::new(&opus_dec, "encoded_audio_in"),
    )?;
    runtime.connect(
        OutputLinkPortRef::new(&opus_dec, "audio_out"),
        InputLinkPortRef::new(&sub_resampler, "audio_in"),
    )?;
    runtime.connect(
        OutputLinkPortRef::new(&sub_resampler, "audio_out"),
        InputLinkPortRef::new(&sub_rechunker, "audio_in"),
    )?;
    runtime.connect(
        OutputLinkPortRef::new(&sub_rechunker, "audio_out"),
        InputLinkPortRef::new(&speaker, "audio"),
    )?;

    // Sensor subscriber — logs received frames, no downstream wiring.
    let _sensor_sub = runtime.add_processor(ProcessorSpec::new(
        processor_type_ref!("tatolab", "moq", "MoqSubscribeTrack"),
        serde_json::json!({ "track_name": "sensor" }),
    ))?;

    tracing::info!("Subscribe side wired: MoQ relay → video display + audio output + sensor log");

    tracing::info!("Starting MoQ roundtrip pipeline... Ctrl+C to stop.");

    runtime.start()?;
    runtime.wait_for_signal()?;

    tracing::info!("MoQ roundtrip stopped");
    Ok(())
}
