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
pub mod audio_window_to_encoded_packet_encoder;
pub mod camera_source;
pub(crate) mod captured_audio_block_hand_off_ring;
pub(crate) mod consecutive_failure_report_schedule;
pub(crate) mod cumulative_count_report_threshold;
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub mod display_window;
#[cfg(test)]
mod emitted_log_line_test_support;
pub mod encoded_audio_packet;
pub mod encoded_frame_to_published_surface_decoder;
pub mod encoded_packet_to_audio_block_decoder;
pub mod encoded_stream_ordering;
pub mod encoded_video_frame;
pub mod h264_decoder;
pub mod h264_encoder;
pub mod h265_decoder;
pub mod h265_encoder;
pub mod h273_color_vui_translation;
pub mod hardware_video_codec_processor_identity;
pub mod microphone_source;
// The MP4 muxer reads its parameter sets through the engine's Vulkan Video
// NAL parser, which MoltenVK cannot serve and which stays Linux-only.
#[cfg(target_os = "linux")]
pub mod mp4_annex_b_access_unit;
#[cfg(target_os = "linux")]
pub mod mp4_fragmented_file_writer;
#[cfg(target_os = "linux")]
pub mod mp4_sink;
#[cfg(target_os = "linux")]
pub mod mp4_track_sample_entry;
#[cfg(test)]
mod msgpack_wire_test_support;
pub mod opus_decoder;
pub mod opus_encoder;
pub mod opus_stream_layout;
pub mod pooled_rgba_frame_staging;
pub(crate) mod processor_thread_join;
pub mod published_surface_to_encoded_frame_encoder;
pub mod speaker_sink;
pub mod test_pattern_source;
pub mod video_frame;
#[cfg(target_os = "linux")]
pub mod virtual_camera_sink;
#[cfg(test)]
mod worker_thread_test_support;

pub use audio_block::{AudioBlock, AudioSampleDtype};
pub use audio_window_to_encoded_packet_encoder::{OpusEncoderApplication, OpusEncoderConfig};
pub use camera_source::{CameraSource, CameraSourceConfig};
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub use display_window::{DisplayWindow, DisplayWindowConfig};
pub use encoded_audio_packet::{
    EncodedAudioCodec, EncodedAudioPacket, EncodedAudioPacketBagRefusal,
    read_encoded_audio_packet_bag,
};
pub use encoded_frame_to_published_surface_decoder::HardwareVideoDecoderConfig;
pub use encoded_stream_ordering::{
    ArrivingEncodedBagDisposition, EncodedStreamOrderingPair, EncodedStreamOrderingPairCounter,
    EncodedStreamSyncPointGate,
};
pub use encoded_video_frame::{
    EncodedVideoCodec, EncodedVideoFrame, EncodedVideoFrameBagRefusal, read_encoded_video_frame_bag,
};
pub use h264_decoder::H264Decoder;
pub use h264_encoder::H264Encoder;
pub use h265_decoder::H265Decoder;
pub use h265_encoder::H265Encoder;
pub use microphone_source::{MicrophoneSource, MicrophoneSourceConfig};
#[cfg(target_os = "linux")]
pub use mp4_sink::Mp4Sink;
pub use opus_decoder::OpusDecoder;
pub use opus_encoder::OpusEncoder;
pub use pooled_rgba_frame_staging::stage_tightly_packed_rgba_into_pooled_pixel_buffer;
pub use published_surface_to_encoded_frame_encoder::HardwareVideoEncoderConfig;
pub use speaker_sink::{SpeakerSink, SpeakerSinkConfig};
pub use test_pattern_source::{TestPatternSource, TestPatternSourceConfig};
pub use video_frame::VideoFrame;
#[cfg(target_os = "linux")]
pub use virtual_camera_sink::{VirtualCameraDoor, VirtualCameraSink, VirtualCameraSinkConfig};

use streamlib::sdk::processors::PROCESSOR_REGISTRY;

/// Register every media built-in on the process-wide registry. In-process
/// static registration (the api-server precedent) — no dlopen, and idempotent,
/// so hosts may call it more than once.
pub fn register_media_builtin_processor_types() {
    PROCESSOR_REGISTRY.register::<test_pattern_source::TestPatternSource::Processor>();
    PROCESSOR_REGISTRY.register::<microphone_source::MicrophoneSource::Processor>();
    PROCESSOR_REGISTRY.register::<speaker_sink::SpeakerSink::Processor>();
    PROCESSOR_REGISTRY.register::<opus_encoder::OpusEncoder::Processor>();
    PROCESSOR_REGISTRY.register::<opus_decoder::OpusDecoder::Processor>();
    #[cfg(target_os = "linux")]
    PROCESSOR_REGISTRY.register::<mp4_sink::Mp4Sink::Processor>();
    PROCESSOR_REGISTRY.register::<camera_source::CameraSource::Processor>();
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    PROCESSOR_REGISTRY.register::<display_window::DisplayWindow::Processor>();
    #[cfg(target_os = "linux")]
    PROCESSOR_REGISTRY.register::<virtual_camera_sink::VirtualCameraSink::Processor>();
    #[cfg(target_os = "linux")]
    PROCESSOR_REGISTRY.register::<h264_encoder::H264Encoder::Processor>();
    #[cfg(target_os = "linux")]
    PROCESSOR_REGISTRY.register::<h264_decoder::H264Decoder::Processor>();
    #[cfg(target_os = "linux")]
    PROCESSOR_REGISTRY.register::<h265_encoder::H265Encoder::Processor>();
    #[cfg(target_os = "linux")]
    PROCESSOR_REGISTRY.register::<h265_decoder::H265Decoder::Processor>();
}
