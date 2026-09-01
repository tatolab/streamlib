// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

#![cfg(target_os = "linux")]

//! The H.264 arm of the codec round trip. The graph, the readers and every
//! assertion live in [`codec_round_trip_harness`], which the H.265 arm runs
//! too; a binary per arm because each stands up its own `App`, and one
//! process brings up one `GpuContext`.

mod codec_round_trip_harness;

use codec_round_trip_harness::CodecRoundTripArm;
use streamlib_media_builtins::{H264Decoder, H264Encoder};

/// #1077, read forwards on H.264.
#[test]
fn every_encoded_h264_frame_the_decoder_is_handed_comes_back_as_a_published_surface() {
    codec_round_trip_harness::every_encoded_frame_the_decoder_is_handed_comes_back_as_a_published_surface(
        CodecRoundTripArm {
            codec_name: "h264",
            encoder_class_import_path: H264Encoder::Processor::processor_class_import_path(),
            decoder_class_import_path: H264Decoder::Processor::processor_class_import_path(),
        },
    );
}
