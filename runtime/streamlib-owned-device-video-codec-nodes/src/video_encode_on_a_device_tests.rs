// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use streamlib_vulkan_video::VideoError;

use crate::video_codec_retry_gate::VideoCodecRefusal;
use streamlib_consumer_rhi::TextureFormat;

use crate::video_encode_on_a_device::{
    the_pixel_buffer_format_the_encoder_reads, the_texture_format_the_encoder_reads,
    why_no_session_was_minted,
};

#[test]
fn a_mint_refused_at_the_video_session_limit_is_waited_out() {
    let not_minted = why_no_session_was_minted(
        "H264Encoder",
        VideoError::VideoSessionLimitReached {
            session_label: "encode/session".to_owned(),
        },
    );
    let VideoCodecRefusal::ForNow(refusal) = not_minted else {
        panic!("the session limit must be waited out: {not_minted:?}");
    };
    assert!(
        refusal.starts_with("H264Encoder: the encoder session could not be minted: "),
        "{refusal}"
    );
}

#[test]
fn a_mint_that_failed_for_any_other_reason_fails_for_good() {
    let not_minted = why_no_session_was_minted(
        "H264Encoder",
        VideoError::BitstreamError("encoder session: out of device memory".to_owned()),
    );
    assert!(
        matches!(not_minted, VideoCodecRefusal::ForGood(_)),
        "{not_minted:?}"
    );
}

/// A texture of a format the encoder does not read is refused naming its format and each the
/// encoder reads, so the node's failure says what to send instead.
#[test]
fn a_texture_of_a_format_the_encoder_does_not_read_is_refused_naming_both() {
    assert_eq!(
        the_texture_format_the_encoder_reads("bgra8_unorm"),
        Ok(TextureFormat::Bgra8Unorm)
    );
    assert_eq!(
        the_texture_format_the_encoder_reads("rgba16_float"),
        Err(
            "is a rgba16_float texture; the encoder reads only rgba8_unorm, rgba8_unorm_srgb, \
             bgra8_unorm, bgra8_unorm_srgb"
                .to_string()
        )
    );
}

#[test]
fn a_pixel_buffer_of_a_format_the_encoder_does_not_read_is_refused_naming_both() {
    assert_eq!(the_pixel_buffer_format_the_encoder_reads("rgba32"), Ok(()));
    assert_eq!(
        the_pixel_buffer_format_the_encoder_reads("bgra32"),
        Err("is a bgra32 pixel buffer; the encoder reads only rgba32".to_string())
    );
}
