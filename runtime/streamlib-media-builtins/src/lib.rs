// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! First-party media built-ins: native pre-built blocks statically linked
//! into the wheel and instantiated from Python by configuration
//! (`stream.add(TestPatternSource)`), whose per-frame paths never enter the
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
pub mod mp4_fragmented_file_writer;
pub mod mp4_sink;
pub mod mp4_track_sample_entry;
#[cfg(test)]
mod msgpack_wire_test_support;
pub mod opus_decoder;
pub mod opus_encoder;
pub mod opus_stream_layout;
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
pub use mp4_sink::Mp4Sink;
pub use opus_decoder::OpusDecoder;
pub use opus_encoder::OpusEncoder;
pub use published_surface_to_encoded_frame_encoder::HardwareVideoEncoderConfig;
pub use speaker_sink::{SpeakerSink, SpeakerSinkConfig};
pub use test_pattern_source::{TestPatternSource, TestPatternSourceConfig};
pub use video_frame::VideoFrame;
#[cfg(target_os = "linux")]
pub use virtual_camera_sink::{VirtualCameraDoor, VirtualCameraSink, VirtualCameraSinkConfig};

use streamlib::sdk::descriptors::ProcessorClassImportPath;
use streamlib::sdk::processors::PROCESSOR_REGISTRY;

/// Each built-in compiled in on some floors only, by its public class name,
/// with the floors it runs on.
const BUILT_INS_COMPILED_IN_ON_SOME_FLOORS_ONLY: &[(&str, &str)] = &[
    ("DisplayWindow", "Linux and macOS"),
    ("VirtualCameraSink", "Linux"),
];

/// Register every media built-in on the process-wide registry, and record each
/// one this floor compiles out so a graph naming it is refused naming the
/// floors it runs on. In-process static registration (the api-server
/// precedent) — no dlopen, and idempotent, so hosts may call it more than once.
pub fn register_media_builtin_processor_types() {
    PROCESSOR_REGISTRY.register::<test_pattern_source::TestPatternSource::Processor>();
    PROCESSOR_REGISTRY.register::<microphone_source::MicrophoneSource::Processor>();
    PROCESSOR_REGISTRY.register::<speaker_sink::SpeakerSink::Processor>();
    PROCESSOR_REGISTRY.register::<opus_encoder::OpusEncoder::Processor>();
    PROCESSOR_REGISTRY.register::<opus_decoder::OpusDecoder::Processor>();
    PROCESSOR_REGISTRY.register::<mp4_sink::Mp4Sink::Processor>();
    PROCESSOR_REGISTRY.register::<camera_source::CameraSource::Processor>();
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    PROCESSOR_REGISTRY.register::<display_window::DisplayWindow::Processor>();
    #[cfg(target_os = "linux")]
    PROCESSOR_REGISTRY.register::<virtual_camera_sink::VirtualCameraSink::Processor>();
    PROCESSOR_REGISTRY.register::<h264_encoder::H264Encoder::Processor>();
    PROCESSOR_REGISTRY.register::<h264_decoder::H264Decoder::Processor>();
    PROCESSOR_REGISTRY.register::<h265_encoder::H265Encoder::Processor>();
    PROCESSOR_REGISTRY.register::<h265_decoder::H265Decoder::Processor>();

    for (class_name, floors_it_runs_on) in BUILT_INS_COMPILED_IN_ON_SOME_FLOORS_ONLY {
        let built_in_node_type = ProcessorClassImportPath::of_built_in_node(class_name)
            .expect("a class name is never blank");
        if !PROCESSOR_REGISTRY.is_registered(&built_in_node_type) {
            PROCESSOR_REGISTRY.register_built_in_node_type_absent_on_this_floor(
                built_in_node_type,
                *floors_it_runs_on,
            );
        }
    }
}

#[cfg(test)]
mod built_in_node_type_tests {
    use streamlib::sdk::error::Error;

    use super::*;

    /// Every built-in this floor registers, as its type and its authored name.
    fn every_built_in_registered_on_this_floor() -> Vec<(ProcessorClassImportPath, &'static str)> {
        vec![
            (
                test_pattern_source::TestPatternSource::processor_class_import_path(),
                test_pattern_source::TestPatternSource::Processor::NAME,
            ),
            (
                microphone_source::MicrophoneSource::processor_class_import_path(),
                microphone_source::MicrophoneSource::Processor::NAME,
            ),
            (
                speaker_sink::SpeakerSink::processor_class_import_path(),
                speaker_sink::SpeakerSink::Processor::NAME,
            ),
            (
                opus_encoder::OpusEncoder::processor_class_import_path(),
                opus_encoder::OpusEncoder::Processor::NAME,
            ),
            (
                opus_decoder::OpusDecoder::processor_class_import_path(),
                opus_decoder::OpusDecoder::Processor::NAME,
            ),
            (
                mp4_sink::Mp4Sink::processor_class_import_path(),
                mp4_sink::Mp4Sink::Processor::NAME,
            ),
            (
                camera_source::CameraSource::processor_class_import_path(),
                camera_source::CameraSource::Processor::NAME,
            ),
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            (
                display_window::DisplayWindow::processor_class_import_path(),
                display_window::DisplayWindow::Processor::NAME,
            ),
            #[cfg(target_os = "linux")]
            (
                virtual_camera_sink::VirtualCameraSink::processor_class_import_path(),
                virtual_camera_sink::VirtualCameraSink::Processor::NAME,
            ),
            (
                h264_encoder::H264Encoder::processor_class_import_path(),
                h264_encoder::H264Encoder::Processor::NAME,
            ),
            (
                h264_decoder::H264Decoder::processor_class_import_path(),
                h264_decoder::H264Decoder::Processor::NAME,
            ),
            (
                h265_encoder::H265Encoder::processor_class_import_path(),
                h265_encoder::H265Encoder::Processor::NAME,
            ),
            (
                h265_decoder::H265Decoder::processor_class_import_path(),
                h265_decoder::H265Decoder::Processor::NAME,
            ),
        ]
    }

    #[test]
    fn every_built_in_is_registered_as_its_class_in_the_stream_package() {
        register_media_builtin_processor_types();

        for (built_in_node_type, authored_name) in every_built_in_registered_on_this_floor() {
            assert_eq!(
                built_in_node_type.as_str(),
                format!("tatolab.stream:{authored_name}")
            );
            assert!(
                PROCESSOR_REGISTRY.is_registered(&built_in_node_type),
                "{built_in_node_type}"
            );
        }
        let rust_module_paths: Vec<ProcessorClassImportPath> = PROCESSOR_REGISTRY
            .registered_processor_class_import_paths()
            .into_iter()
            .filter(|path| path.as_str().starts_with("streamlib_media_builtins::"))
            .collect();
        assert!(
            rust_module_paths.is_empty(),
            "a built-in's Rust module path reached the registry: {rust_module_paths:?}"
        );
    }

    /// The table names classes by hand, for floors where they are not compiled
    /// in; each must still be the class a floor that compiles it registers.
    #[test]
    fn every_built_in_on_some_floors_only_is_registered_here_or_refused_naming_where_it_runs() {
        register_media_builtin_processor_types();
        let registered_here: Vec<ProcessorClassImportPath> =
            every_built_in_registered_on_this_floor()
                .into_iter()
                .map(|(built_in_node_type, _)| built_in_node_type)
                .collect();

        for (class_name, floors_it_runs_on) in BUILT_INS_COMPILED_IN_ON_SOME_FLOORS_ONLY {
            let built_in_node_type =
                ProcessorClassImportPath::of_built_in_node(class_name).unwrap();
            let refusal = PROCESSOR_REGISTRY
                .refuse_a_built_in_node_type_absent_on_this_floor(&built_in_node_type);
            if registered_here.contains(&built_in_node_type) {
                assert!(refusal.is_ok(), "{refusal:?}");
            } else {
                match refusal {
                    Err(Error::BuiltInNodeTypeAbsentOnThisFloor {
                        floors_it_runs_on: refused_naming,
                        ..
                    }) => assert_eq!(refused_naming, *floors_it_runs_on),
                    other => {
                        panic!("{built_in_node_type}: expected the floor refusal, got {other:?}")
                    }
                }
            }
        }
        #[cfg(target_os = "linux")]
        assert!(
            BUILT_INS_COMPILED_IN_ON_SOME_FLOORS_ONLY
                .iter()
                .all(|(class_name, _)| {
                    registered_here
                        .contains(&ProcessorClassImportPath::of_built_in_node(class_name).unwrap())
                }),
            "Linux compiles in every built-in, so every name the table holds is one it registers"
        );
    }
}
