// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::time::Duration;

use super::*;
use crate::core::child_process_restart_backoff::{
    FIRST_CHILD_PROCESS_RESTART_WAIT, LONGEST_CHILD_PROCESS_RESTART_WAIT,
};

/// The first refusal seen waits its first wait, and each retry that leaves a link refused waits
/// twice as long up to the longest, though the far side holds no refused link while it awaits
/// the answer to each retry; once it has held none for the longest wait, the waits start over.
///
/// Mental-revert: retrying at each turn of the host loop compiles the stream ten times a
/// second for a link that cannot be wired; starting over on a turn between a retry and its
/// answer retries such a link every second or two; never starting over leaves a later refusal
/// waiting the longest wait.
#[test]
fn a_link_refused_again_after_each_retry_waits_longer_and_starts_over_once_none_is_refused() {
    let mut retries = RetriesOfTheLinksARunningFarSideRefused::new();
    let start = Instant::now();
    let at = |millis: u64| start + Duration::from_millis(millis);

    assert!(!retries.a_retry_is_due(true, at(0)));
    assert!(!retries.a_retry_is_due(true, at(999)));
    assert!(retries.a_retry_is_due(true, at(1_000)));
    assert!(
        !retries.a_retry_is_due(false, at(1_100)),
        "awaiting the retry's answer"
    );
    assert!(!retries.a_retry_is_due(true, at(1_200)), "refused again");
    assert!(!retries.a_retry_is_due(true, at(2_999)));
    assert!(retries.a_retry_is_due(true, at(3_000)));
    assert!(!retries.a_retry_is_due(false, at(3_100)));
    assert!(!retries.a_retry_is_due(true, at(6_999)));
    assert!(retries.a_retry_is_due(true, at(7_000)));

    let mut retried_at = 7_000;
    let longest = LONGEST_CHILD_PROCESS_RESTART_WAIT.as_millis() as u64;
    for _ in 0..6 {
        retried_at += longest;
        assert!(!retries.a_retry_is_due(false, at(retried_at - longest / 2)));
        assert!(retries.a_retry_is_due(true, at(retried_at)));
    }
    assert!(!retries.a_retry_is_due(true, at(retried_at + longest - 1)));

    let last_refused_at = retried_at + longest - 1;
    assert!(!retries.a_retry_is_due(false, at(last_refused_at + longest - 1)));
    assert!(
        retries.a_retry_is_due(true, at(last_refused_at + longest)),
        "held none for less than the longest wait, so the retry it was due is due"
    );

    let opened_at = last_refused_at + longest;
    assert!(!retries.a_retry_is_due(false, at(opened_at + longest)));
    assert!(!retries.a_retry_is_due(true, at(opened_at + longest + 1)));
    let first = FIRST_CHILD_PROCESS_RESTART_WAIT.as_millis() as u64;
    assert!(retries.a_retry_is_due(true, at(opened_at + longest + 1 + first)));
}
