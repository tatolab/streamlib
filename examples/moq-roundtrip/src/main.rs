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

use streamlib::sdk::error::Result;
use streamlib::sdk::processors::{input, output};
use streamlib::sdk::runtime::Runner;
use streamlib_audio::{
    AudioCaptureProcessor, AudioOutputProcessor, AudioResamplerProcessor,
    BufferRechunkerProcessor, ChordGeneratorProcessor,
};
use streamlib_camera::CameraProcessor;
use streamlib_display::DisplayProcessor;
use streamlib_h264::{H264DecoderProcessor, H264EncoderProcessor};
use streamlib_moq::{MoqPublishTrackProcessor, MoqSubscribeTrackProcessor};
use streamlib_opus::{OpusDecoderProcessor, OpusEncoderProcessor};

fn main() -> Result<()> {
    rustls::crypto::ring::default_provider()
        .install_default()
        .ok();

    tracing::info!("=== MoQ Roundtrip ===");
    tracing::info!("Publishing: camera (H.264) + audio (Opus) + sensor data");
    tracing::info!("Subscribing: video → display, audio → speaker, sensor → log");

    let runtime = Runner::new()?;

    // Register the `@tatolab/core` wire vocabulary so iceoryx2 publishers
    // honor each schema's `max_payload_bytes` instead of falling back to
    // the 64 KiB default (which drops the encoder's first IDR).
    runtime.load_project(env!("CARGO_MANIFEST_DIR"))?;

    // ---- PUBLISH SIDE ----

    let camera = runtime.add_processor(CameraProcessor::node(Default::default()))?;
    let h264_enc = runtime.add_processor(H264EncoderProcessor::node(
        H264EncoderProcessor::Config {
            keyframe_interval_seconds: Some(2.0),
            ..Default::default()
        },
    ))?;
    let video_pub = runtime.add_processor(MoqPublishTrackProcessor::node(
        MoqPublishTrackProcessor::Config {
            track_name: Some("video".to_string()),
        },
    ))?;

    runtime.connect(
        output::<CameraProcessor::OutputLink::video>(&camera),
        input::<H264EncoderProcessor::InputLink::video_in>(&h264_enc),
    )?;
    runtime.connect(
        output::<H264EncoderProcessor::OutputLink::encoded_video_out>(&h264_enc),
        input::<MoqPublishTrackProcessor::InputLink::data_in>(&video_pub),
    )?;

    let mic = runtime.add_processor(AudioCaptureProcessor::node(Default::default()))?;
    let resampler = runtime.add_processor(AudioResamplerProcessor::node(
        AudioResamplerProcessor::Config {
            target_sample_rate: 48000,
            ..Default::default()
        },
    ))?;
    let rechunker = runtime.add_processor(BufferRechunkerProcessor::node(
        BufferRechunkerProcessor::Config {
            target_buffer_size: 960,
        },
    ))?;
    let opus_enc = runtime.add_processor(OpusEncoderProcessor::node(Default::default()))?;
    let audio_pub = runtime.add_processor(MoqPublishTrackProcessor::node(
        MoqPublishTrackProcessor::Config {
            track_name: Some("audio".to_string()),
        },
    ))?;

    runtime.connect(
        output::<AudioCaptureProcessor::OutputLink::audio>(&mic),
        input::<AudioResamplerProcessor::InputLink::audio_in>(&resampler),
    )?;
    runtime.connect(
        output::<AudioResamplerProcessor::OutputLink::audio_out>(&resampler),
        input::<BufferRechunkerProcessor::InputLink::audio_in>(&rechunker),
    )?;
    runtime.connect(
        output::<BufferRechunkerProcessor::OutputLink::audio_out>(&rechunker),
        input::<OpusEncoderProcessor::InputLink::audio_in>(&opus_enc),
    )?;
    runtime.connect(
        output::<OpusEncoderProcessor::OutputLink::encoded_audio_out>(&opus_enc),
        input::<MoqPublishTrackProcessor::InputLink::data_in>(&audio_pub),
    )?;

    let sensor = runtime.add_processor(ChordGeneratorProcessor::node(
        ChordGeneratorProcessor::Config {
            amplitude: 0.1,
            buffer_size: 128,
            sample_rate: 8000,
        },
    ))?;
    let sensor_pub = runtime.add_processor(MoqPublishTrackProcessor::node(
        MoqPublishTrackProcessor::Config {
            track_name: Some("sensor".to_string()),
        },
    ))?;

    runtime.connect(
        output::<ChordGeneratorProcessor::OutputLink::chord>(&sensor),
        input::<MoqPublishTrackProcessor::InputLink::data_in>(&sensor_pub),
    )?;

    tracing::info!("Publish side wired: camera + audio + sensor → MoQ relay");

    // ---- SUBSCRIBE SIDE ----

    let video_sub = runtime.add_processor(MoqSubscribeTrackProcessor::node(
        MoqSubscribeTrackProcessor::Config {
            track_name: "video".to_string(),
        },
    ))?;
    let h264_dec = runtime.add_processor(H264DecoderProcessor::node(Default::default()))?;
    let display = runtime.add_processor(DisplayProcessor::node(DisplayProcessor::Config {
        width: 1280,
        height: 720,
        title: Some("MoQ Roundtrip".to_string()),
        ..Default::default()
    }))?;

    runtime.connect(
        output::<MoqSubscribeTrackProcessor::OutputLink::data_out>(&video_sub),
        input::<H264DecoderProcessor::InputLink::encoded_video_in>(&h264_dec),
    )?;
    runtime.connect(
        output::<H264DecoderProcessor::OutputLink::video_out>(&h264_dec),
        input::<DisplayProcessor::InputLink::video>(&display),
    )?;

    let audio_sub = runtime.add_processor(MoqSubscribeTrackProcessor::node(
        MoqSubscribeTrackProcessor::Config {
            track_name: "audio".to_string(),
        },
    ))?;
    let opus_dec = runtime.add_processor(OpusDecoderProcessor::node(Default::default()))?;
    let sub_resampler = runtime.add_processor(AudioResamplerProcessor::node(
        AudioResamplerProcessor::Config {
            target_sample_rate: 44100,
            ..Default::default()
        },
    ))?;
    let sub_rechunker = runtime.add_processor(BufferRechunkerProcessor::node(
        BufferRechunkerProcessor::Config {
            target_buffer_size: 512,
        },
    ))?;
    let speaker = runtime.add_processor(AudioOutputProcessor::node(Default::default()))?;

    runtime.connect(
        output::<MoqSubscribeTrackProcessor::OutputLink::data_out>(&audio_sub),
        input::<OpusDecoderProcessor::InputLink::encoded_audio_in>(&opus_dec),
    )?;
    runtime.connect(
        output::<OpusDecoderProcessor::OutputLink::audio_out>(&opus_dec),
        input::<AudioResamplerProcessor::InputLink::audio_in>(&sub_resampler),
    )?;
    runtime.connect(
        output::<AudioResamplerProcessor::OutputLink::audio_out>(&sub_resampler),
        input::<BufferRechunkerProcessor::InputLink::audio_in>(&sub_rechunker),
    )?;
    runtime.connect(
        output::<BufferRechunkerProcessor::OutputLink::audio_out>(&sub_rechunker),
        input::<AudioOutputProcessor::InputLink::audio>(&speaker),
    )?;

    // Sensor subscriber — logs received frames, no downstream wiring.
    let _sensor_sub = runtime.add_processor(MoqSubscribeTrackProcessor::node(
        MoqSubscribeTrackProcessor::Config {
            track_name: "sensor".to_string(),
        },
    ))?;

    tracing::info!("Subscribe side wired: MoQ relay → video display + audio output + sensor log");

    tracing::info!("Starting MoQ roundtrip pipeline... Ctrl+C to stop.");

    runtime.start()?;
    runtime.wait_for_signal()?;

    tracing::info!("MoQ roundtrip stopped");
    Ok(())
}
