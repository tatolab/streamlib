// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::time::{Duration, Instant};

use crate::video_codec_retry_gate::{VIDEO_CODEC_RETRY_INTERVAL, VideoCodecRetryGate};

#[test]
fn a_try_is_made_at_once_by_a_node_that_has_not_made_one() {
    let gate = VideoCodecRetryGate::default();
    assert!(gate.a_try_may_be_made_at(Instant::now()));
}

#[test]
fn a_try_refused_for_now_is_made_again_once_the_retry_interval_has_passed() {
    let mut gate = VideoCodecRetryGate::default();
    let refused_at = Instant::now();
    assert_eq!(gate.refused_for_now_at(refused_at), 1);
    assert!(!gate.a_try_may_be_made_at(refused_at));
    assert!(
        !gate.a_try_may_be_made_at(
            refused_at + VIDEO_CODEC_RETRY_INTERVAL - Duration::from_millis(1)
        )
    );
    assert!(gate.a_try_may_be_made_at(refused_at + VIDEO_CODEC_RETRY_INTERVAL));
}

#[test]
fn a_try_that_succeeded_reports_the_refusals_before_it_and_the_next_refusal_counts_from_one() {
    let mut gate = VideoCodecRetryGate::default();
    let first_refusal = Instant::now();
    gate.refused_for_now_at(first_refusal);
    let second_refusal = first_refusal + VIDEO_CODEC_RETRY_INTERVAL;
    assert_eq!(gate.refused_for_now_at(second_refusal), 2);
    assert_eq!(gate.succeeded(), 2);
    assert!(gate.a_try_may_be_made_at(second_refusal));
    assert_eq!(gate.refused_for_now_at(second_refusal), 1);
}

#[test]
fn a_try_that_failed_for_good_is_never_made_again() {
    let mut gate = VideoCodecRetryGate::default();
    gate.failed_for_good();
    assert!(!gate.a_try_may_be_made_at(Instant::now() + Duration::from_secs(3600)));
}
