// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::time::{Duration, Instant};

use crate::video_encode_session_limit_wait::{
    ENCODE_SESSION_LIMIT_MINT_RETRY_INTERVAL, WaitForAVideoSessionUnderTheLimit,
};

#[test]
fn a_mint_is_tried_at_once_when_the_limit_has_refused_none() {
    let wait = WaitForAVideoSessionUnderTheLimit::default();
    assert!(wait.a_mint_may_be_tried_at(Instant::now()));
}

#[test]
fn a_refused_mint_is_tried_again_once_the_retry_interval_has_passed() {
    let mut wait = WaitForAVideoSessionUnderTheLimit::default();
    let refused_at = Instant::now();
    assert_eq!(wait.mint_refused_at(refused_at), 1);
    assert!(!wait.a_mint_may_be_tried_at(refused_at));
    assert!(!wait.a_mint_may_be_tried_at(
        refused_at + ENCODE_SESSION_LIMIT_MINT_RETRY_INTERVAL - Duration::from_millis(1)
    ));
    assert!(wait.a_mint_may_be_tried_at(refused_at + ENCODE_SESSION_LIMIT_MINT_RETRY_INTERVAL));
}

#[test]
fn a_mint_that_succeeds_reports_the_refusals_before_it_and_waits_no_longer() {
    let mut wait = WaitForAVideoSessionUnderTheLimit::default();
    let first_refusal = Instant::now();
    wait.mint_refused_at(first_refusal);
    let second_refusal = first_refusal + ENCODE_SESSION_LIMIT_MINT_RETRY_INTERVAL;
    assert_eq!(wait.mint_refused_at(second_refusal), 2);
    assert_eq!(wait.mint_succeeded(), 2);
    assert!(wait.a_mint_may_be_tried_at(second_refusal));
    assert_eq!(wait.mint_refused_at(second_refusal), 1);
}
