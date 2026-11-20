// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use streamlib_vulkan_video::VideoError;

use crate::video_encode_on_a_device::{EncodeSessionNotMinted, why_no_session_was_minted};

#[test]
fn a_mint_refused_at_the_video_session_limit_is_waited_out() {
    let not_minted = why_no_session_was_minted(
        "H264Encoder",
        VideoError::VideoSessionLimitReached {
            session_label: "encode/session".to_owned(),
        },
    );
    let EncodeSessionNotMinted::AtTheVideoSessionLimit(refusal) = not_minted else {
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
        matches!(not_minted, EncodeSessionNotMinted::Failed(_)),
        "{not_minted:?}"
    );
}
