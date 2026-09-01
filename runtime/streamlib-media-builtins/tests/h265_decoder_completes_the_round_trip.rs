// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

#![cfg(target_os = "linux")]

//! The H.265 arm of the codec round trip, and the executable form of the CTU
//! crop: a 1920x1080 pattern is coded at 1920x1088, and the harness's extent
//! assertion is what fails if the conformance window stops reaching the
//! frames the decoder publishes.
//!
//! Everything else lives in [`codec_round_trip_harness`], which the H.264 arm
//! runs too; a binary per arm because each stands up its own `App`, and one
//! process brings up one `GpuContext`.

mod codec_round_trip_harness;

use codec_round_trip_harness::CodecRoundTripArm;
use streamlib_media_builtins::{H265Decoder, H265Encoder};

/// The H.265 round trip, including the pad the H.265 encoder writes a
/// conformance window for.
#[test]
fn every_encoded_h265_frame_the_decoder_is_handed_comes_back_as_a_published_surface() {
    codec_round_trip_harness::every_encoded_frame_the_decoder_is_handed_comes_back_as_a_published_surface(
        CodecRoundTripArm {
            codec_name: "h265",
            encoder_class_import_path: H265Encoder::Processor::processor_class_import_path(),
            decoder_class_import_path: H265Decoder::Processor::processor_class_import_path(),
        },
    );
}
