// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::time::{Duration, Instant};

use crate::video_encode_session_mint_gate::{
    ENCODE_SESSION_LIMIT_MINT_RETRY_INTERVAL, EncodeSessionMintGate,
};

#[test]
fn a_mint_is_tried_at_once_by_an_encoder_that_has_not_tried_one() {
    let gate = EncodeSessionMintGate::default();
    assert!(gate.a_mint_may_be_tried_at(Instant::now()));
}

#[test]
fn a_mint_the_session_limit_refused_is_tried_again_once_the_retry_interval_has_passed() {
    let mut gate = EncodeSessionMintGate::default();
    let refused_at = Instant::now();
    assert_eq!(gate.refused_at_the_session_limit_at(refused_at), 1);
    assert!(!gate.a_mint_may_be_tried_at(refused_at));
    assert!(!gate.a_mint_may_be_tried_at(
        refused_at + ENCODE_SESSION_LIMIT_MINT_RETRY_INTERVAL - Duration::from_millis(1)
    ));
    assert!(gate.a_mint_may_be_tried_at(refused_at + ENCODE_SESSION_LIMIT_MINT_RETRY_INTERVAL));
}

#[test]
fn a_minted_session_reports_the_refusals_before_it_and_the_next_refusal_counts_from_one() {
    let mut gate = EncodeSessionMintGate::default();
    let first_refusal = Instant::now();
    gate.refused_at_the_session_limit_at(first_refusal);
    let second_refusal = first_refusal + ENCODE_SESSION_LIMIT_MINT_RETRY_INTERVAL;
    assert_eq!(gate.refused_at_the_session_limit_at(second_refusal), 2);
    assert_eq!(gate.minted(), 2);
    assert!(gate.a_mint_may_be_tried_at(second_refusal));
    assert_eq!(gate.refused_at_the_session_limit_at(second_refusal), 1);
}

#[test]
fn a_mint_that_failed_for_good_is_never_tried_again() {
    let mut gate = EncodeSessionMintGate::default();
    gate.failed_for_good();
    assert!(!gate.a_mint_may_be_tried_at(Instant::now() + Duration::from_secs(3600)));
}
