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
//! All MoQ config is automatic — Cloudflare draft-14 relay, auto-generated namespace.
//!
//! Usage:
//!   cargo run -p moq-roundtrip

use streamlib::{
    input, output,
    // Sources
    CameraProcessor, AudioCaptureProcessor, ChordGeneratorProcessor,
    // Codecs + audio processing
    H264EncoderProcessor, H264DecoderProcessor,
    OpusEncoderProcessor, OpusDecoderProcessor,
    AudioResamplerProcessor, BufferRechunkerProcessor,
    // MoQ transport
    MoqPublishTrackConfig, MoqPublishTrackProcessor,
    MoqSubscribeTrackConfig, MoqSubscribeTrackProcessor,
    // Sinks
    DisplayProcessor, AudioOutputProcessor,
    // Runtime
    Result, StreamRuntime,
};
use streamlib::_generated_::{
    AudioResamplerConfig, BufferRechunkerConfig, ChordGeneratorConfig, DisplayConfig,
    H264EncoderConfig,
};

fn main() -> Result<()> {
    rustls::crypto::ring::default_provider()
        .install_default()
        .ok();

    tracing::info!("=== MoQ Roundtrip - StreamLib Edition ===");
    tracing::info!("Publishing: camera (H.264) + audio (Opus) + sensor data");
    tracing::info!("Subscribing: video → display, audio → speaker, sensor → log");

    let runtime = StreamRuntime::new()?;

    // =========================================================================
    // PUBLISH SIDE
    // =========================================================================

    // Video: Camera → H264 Encoder → MoQ Publish
    let camera = runtime.add_processor(CameraProcessor::Processor::node(Default::default()))?;
    let h264_enc = runtime.add_processor(H264EncoderProcessor::Processor::node(H264EncoderConfig {
        keyframe_interval: Some(10),
        ..Default::default()
    }))?;
    let video_pub = runtime.add_processor(MoqPublishTrackProcessor::Processor::node(
        MoqPublishTrackConfig { track_name: Some("video".to_string()) },
    ))?;

    runtime.connect(
        output::<CameraProcessor::OutputLink::video>(&camera),
        input::<H264EncoderProcessor::InputLink::video_in>(&h264_enc),
    )?;
    runtime.connect(
        output::<H264EncoderProcessor::OutputLink::encoded_video_out>(&h264_enc),
        input::<MoqPublishTrackProcessor::InputLink::data_in>(&video_pub),
    )?;

    // Audio: AudioCapture → Resampler (48kHz) → Rechunker (960 samples) → Opus Encoder → MoQ Publish
    let mic = runtime.add_processor(AudioCaptureProcessor::Processor::node(Default::default()))?;
    let resampler = runtime.add_processor(AudioResamplerProcessor::Processor::node(
        AudioResamplerConfig {
            target_sample_rate: 48000,
            ..Default::default()
        },
    ))?;
    let rechunker = runtime.add_processor(BufferRechunkerProcessor::Processor::node(
        BufferRechunkerConfig {
            target_buffer_size: 960,
        },
    ))?;
    let opus_enc = runtime.add_processor(OpusEncoderProcessor::Processor::node(Default::default()))?;
    let audio_pub = runtime.add_processor(MoqPublishTrackProcessor::Processor::node(
        MoqPublishTrackConfig { track_name: Some("audio".to_string()) },
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

    // Sensor: ChordGenerator (simulating sensor telemetry) → MoQ Publish
    let sensor = runtime.add_processor(ChordGeneratorProcessor::Processor::node(
        ChordGeneratorConfig {
            amplitude: 0.1,
            buffer_size: 128,
            sample_rate: 8000,
        },
    ))?;
    let sensor_pub = runtime.add_processor(MoqPublishTrackProcessor::Processor::node(
        MoqPublishTrackConfig { track_name: Some("sensor".to_string()) },
    ))?;

    runtime.connect(
        output::<ChordGeneratorProcessor::OutputLink::chord>(&sensor),
        input::<MoqPublishTrackProcessor::InputLink::data_in>(&sensor_pub),
    )?;

    tracing::info!("Publish side: camera + audio + sensor → MoQ relay");

    // =========================================================================
    // SUBSCRIBE SIDE
    // =========================================================================

    // Video: MoQ Subscribe → H264 Decoder → Display
    let video_sub = runtime.add_processor(MoqSubscribeTrackProcessor::Processor::node(
        MoqSubscribeTrackConfig { track_name: "video".to_string() },
    ))?;
    let h264_dec = runtime.add_processor(H264DecoderProcessor::Processor::node(Default::default()))?;
    let display = runtime.add_processor(DisplayProcessor::Processor::node(DisplayConfig {
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

    // Audio: MoQ Subscribe → Opus Decoder → Resampler (44100Hz) → Rechunker (512) → Audio Output
    let audio_sub = runtime.add_processor(MoqSubscribeTrackProcessor::Processor::node(
        MoqSubscribeTrackConfig { track_name: "audio".to_string() },
    ))?;
    let opus_dec = runtime.add_processor(OpusDecoderProcessor::Processor::node(Default::default()))?;
    let sub_resampler = runtime.add_processor(AudioResamplerProcessor::Processor::node(
        AudioResamplerConfig {
            target_sample_rate: 44100,
            ..Default::default()
        },
    ))?;
    let sub_rechunker = runtime.add_processor(BufferRechunkerProcessor::Processor::node(
        BufferRechunkerConfig {
            target_buffer_size: 512,
        },
    ))?;
    let speaker = runtime.add_processor(AudioOutputProcessor::Processor::node(Default::default()))?;

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

    // Sensor: MoQ Subscribe (logs received frames, not wired to output)
    let _sensor_sub = runtime.add_processor(MoqSubscribeTrackProcessor::Processor::node(
        MoqSubscribeTrackConfig { track_name: "sensor".to_string() },
    ))?;

    tracing::info!("Subscribe side: MoQ relay → video display + audio output + sensor log");

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
