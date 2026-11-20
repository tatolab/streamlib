// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use streamlib_consumer_rhi::ConsumerRhiError;

use crate::video_codec_retry_gate::VideoCodecRefusal;
use crate::video_decode_on_a_device::why_the_decode_device_was_refused;

#[test]
fn a_decode_device_the_gpu_lacked_a_resource_for_is_refused_for_now_by_name() {
    let refusal = why_the_decode_device_was_refused(
        "H264Decoder",
        ConsumerRhiError::GpuOutOfResourcesForNow(
            "Failed to create consumer logical device: ERROR_INITIALIZATION_FAILED".to_owned(),
        ),
    );
    let VideoCodecRefusal::ForNow(refusal) = refusal else {
        panic!("refused for good: {refusal:?}");
    };
    assert!(
        refusal.starts_with("H264Decoder: the Vulkan Video decode device was refused: "),
        "{refusal}"
    );
}

#[test]
fn a_decode_device_the_gpu_cannot_give_is_refused_for_good() {
    let refusal = why_the_decode_device_was_refused(
        "H265Decoder",
        ConsumerRhiError::Gpu("no video decode queue family".to_owned()),
    );
    assert!(
        matches!(refusal, VideoCodecRefusal::ForGood(_)),
        "{refusal:?}"
    );
}
