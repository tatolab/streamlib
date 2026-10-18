// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

#![cfg(any(target_os = "linux", target_os = "macos"))]

//! The H.265 arm of the lossy round trip: a burst the encoded link cannot
//! hold, survived by discarding to the next keyframe. The graph and every
//! assertion live in [`codec_round_trip_harness`]; a binary of its own
//! because one process brings up one `GpuContext`.

mod codec_round_trip_harness;

use codec_round_trip_harness::CodecRoundTripArm;
use streamlib_media_builtins::{H265Decoder, H265Encoder};

#[test]
fn a_loss_on_the_h265_link_is_survived_by_discarding_to_the_next_keyframe() {
    codec_round_trip_harness::a_loss_on_the_encoded_link_is_survived_by_discarding_to_the_next_keyframe(
        CodecRoundTripArm {
            codec_name: "h265",
            encoder_class_import_path: H265Encoder::Processor::processor_class_import_path(),
            decoder_class_import_path: H265Decoder::Processor::processor_class_import_path(),
        },
    );
}
