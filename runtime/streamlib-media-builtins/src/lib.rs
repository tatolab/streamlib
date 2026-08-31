// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! First-party media built-ins: native pre-built blocks statically linked
//! into the wheel and instantiated from Python by configuration
//! (`rt.add(TestPatternSource)`), whose per-frame paths never enter the
//! interpreter.
//!
//! Written against the SDK's handle-shaped primitives only — pixel-buffer
//! pool, texture cache, present target — never private engine guts.

pub mod audio_block;
pub(crate) mod audio_samples_awaiting_playback_ring;
#[cfg(target_os = "linux")]
pub mod camera_source;
pub(crate) mod captured_audio_block_hand_off_ring;
pub(crate) mod consecutive_failure_report_schedule;
pub(crate) mod cumulative_count_report_threshold;
#[cfg(target_os = "linux")]
pub mod display_window;
#[cfg(test)]
mod emitted_log_line_test_support;
pub mod encoded_video_frame;
#[cfg(target_os = "linux")]
pub mod h264_decoder;
#[cfg(target_os = "linux")]
pub mod h264_encoder;
#[cfg(target_os = "linux")]
pub mod h273_color_vui_translation;
pub mod microphone_source;
#[cfg(test)]
mod msgpack_wire_test_support;
pub(crate) mod processor_thread_join;
pub mod speaker_sink;
pub mod test_pattern_source;
#[cfg(target_os = "linux")]
pub mod v4l2_color;
pub mod video_frame;
#[cfg(test)]
mod worker_thread_test_support;

pub use audio_block::{AudioBlock, AudioSampleDtype};
#[cfg(target_os = "linux")]
pub use camera_source::{CameraSource, CameraSourceConfig};
#[cfg(target_os = "linux")]
pub use display_window::{DisplayWindow, DisplayWindowConfig};
pub use encoded_video_frame::{
    EncodedFrameOrderingPair, EncodedFrameOrderingPairCounter, EncodedVideoCodec,
    EncodedVideoFrame, EncodedVideoFrameBagRefusal, read_encoded_video_frame_bag,
};
#[cfg(target_os = "linux")]
pub use h264_decoder::{H264Decoder, H264DecoderConfig};
#[cfg(target_os = "linux")]
pub use h264_encoder::{H264Encoder, H264EncoderConfig};
pub use microphone_source::{MicrophoneSource, MicrophoneSourceConfig};
pub use speaker_sink::{SpeakerSink, SpeakerSinkConfig};
pub use test_pattern_source::{TestPatternSource, TestPatternSourceConfig};
pub use video_frame::VideoFrame;

use streamlib::sdk::processors::PROCESSOR_REGISTRY;

/// Register every media built-in on the process-wide registry. In-process
/// static registration (the api-server precedent) — no dlopen, and idempotent,
/// so hosts may call it more than once.
pub fn register_media_builtin_processor_types() {
    PROCESSOR_REGISTRY.register::<test_pattern_source::TestPatternSource::Processor>();
    PROCESSOR_REGISTRY.register::<microphone_source::MicrophoneSource::Processor>();
    PROCESSOR_REGISTRY.register::<speaker_sink::SpeakerSink::Processor>();
    #[cfg(target_os = "linux")]
    PROCESSOR_REGISTRY.register::<camera_source::CameraSource::Processor>();
    #[cfg(target_os = "linux")]
    PROCESSOR_REGISTRY.register::<display_window::DisplayWindow::Processor>();
    #[cfg(target_os = "linux")]
    PROCESSOR_REGISTRY.register::<h264_encoder::H264Encoder::Processor>();
    #[cfg(target_os = "linux")]
    PROCESSOR_REGISTRY.register::<h264_decoder::H264Decoder::Processor>();
}
