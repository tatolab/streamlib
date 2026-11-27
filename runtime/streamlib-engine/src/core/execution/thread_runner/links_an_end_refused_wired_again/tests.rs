// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::time::Duration;

use super::*;
use crate::core::child_process_restart_backoff::{
    FIRST_CHILD_PROCESS_RESTART_WAIT, LONGEST_CHILD_PROCESS_RESTART_WAIT,
};

/// The first refusal seen waits its first wait, each retry that leaves a link refused waits
/// twice as long up to the longest, and a far side holding none starts the waits over.
///
/// Mental-revert: retrying at each turn of the host loop compiles the stream ten times a
/// second for a link that cannot be wired; never starting over leaves a later refusal
/// waiting the longest wait.
#[test]
fn a_refused_link_is_retried_after_waits_that_double_and_start_over_once_none_is_refused() {
    let mut retries = RetriesOfTheLinksARunningFarSideRefused::new();
    let seen_at = Instant::now();

    assert!(!retries.a_retry_is_due(true, seen_at));
    assert!(!retries.a_retry_is_due(true, seen_at + Duration::from_millis(999)));
    assert!(retries.a_retry_is_due(true, seen_at + Duration::from_secs(1)));
    assert!(!retries.a_retry_is_due(true, seen_at + Duration::from_secs(2)));
    assert!(retries.a_retry_is_due(true, seen_at + Duration::from_secs(3)));

    let mut retried_at = seen_at + Duration::from_secs(3);
    for _ in 0..8 {
        retried_at += LONGEST_CHILD_PROCESS_RESTART_WAIT;
        assert!(retries.a_retry_is_due(true, retried_at));
    }
    assert!(!retries.a_retry_is_due(
        true,
        retried_at + LONGEST_CHILD_PROCESS_RESTART_WAIT - Duration::from_millis(1)
    ));

    assert!(!retries.a_retry_is_due(false, retried_at));
    assert!(!retries.a_retry_is_due(true, retried_at));
    assert!(retries.a_retry_is_due(true, retried_at + FIRST_CHILD_PROCESS_RESTART_WAIT));
}
