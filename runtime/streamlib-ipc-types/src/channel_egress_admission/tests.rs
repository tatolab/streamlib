// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use crate::*;

#[test]
fn channel_trust_tier_defaults_and_labels() {
    assert_eq!(
        ChannelTrustTier::Trusted.default_chunk_ceiling_bytes(),
        TRUSTED_CHANNEL_CHUNK_CEILING_BYTES
    );
    assert_eq!(
        ChannelTrustTier::UntrustedSession.default_chunk_ceiling_bytes(),
        UNTRUSTED_SESSION_CHANNEL_CHUNK_CEILING_BYTES
    );
    assert!(
        ChannelTrustTier::UntrustedSession.default_chunk_ceiling_bytes()
            < ChannelTrustTier::Trusted.default_chunk_ceiling_bytes(),
        "untrusted-session ceiling must be tighter than trusted"
    );
    assert_eq!(ChannelTrustTier::Trusted.as_str(), "trusted");
    assert_eq!(
        ChannelTrustTier::UntrustedSession.as_str(),
        "untrusted-session"
    );
}

/// The logging half of the same accounting: a frame that fits a power of two
/// exactly still needs a sample larger than it, so the segment it grows to is
/// the next bucket up. Measuring the frame alone under-reports by 2× here.
#[test]
fn a_growth_reports_the_bucket_the_sample_takes_and_not_the_frames_own_rounding() {
    let mut slot = 4096usize;
    match decide_channel_egress_admission(65_536, 128 * 1024, &mut slot, || {
        panic!("an admitted frame records no refusal")
    }) {
        ChannelEgressAdmission::Admitted {
            grew_to: Some(growth),
        } => assert_eq!(
            growth.new_segment_bytes, 131_072,
            "a 64 KiB frame's sample is past 64 KiB, so its bucket is 128 KiB"
        ),
        other => panic!("expected an Admitted growth, got {other:?}"),
    }
}

#[test]
fn egress_admission_refuses_over_ceiling_and_counts() {
    let ceiling = 128 * 1024usize;
    let mut refused = 0u64;
    let mut slot = 64usize;
    // First over-ceiling frame: refused, count → 1, slot untouched.
    assert_eq!(
        decide_channel_egress_admission(ceiling + 1, ceiling, &mut slot, || {
            refused += 1;
            refused
        }),
        ChannelEgressAdmission::RefusedOverCeiling { refused_count: 1 }
    );
    assert_eq!(slot, 64, "a refusal must not grow the tracked slot");
    // Second over-ceiling frame: count keeps climbing.
    assert_eq!(
        decide_channel_egress_admission(ceiling + 999, ceiling, &mut slot, || {
            refused += 1;
            refused
        }),
        ChannelEgressAdmission::RefusedOverCeiling { refused_count: 2 }
    );
}

#[test]
fn egress_admission_grows_without_crossing_quarter_ceiling() {
    let ceiling = 128 * 1024usize; // quarter = 32 KiB
    let mut refused = 0u64;
    let mut slot = 4096usize;
    // A frame that grows the slot but stays at or below the quarter ceiling
    // (32 KiB) must NOT flag a crossing. 20_000 → next_pow2 = 32_768 == quarter.
    match decide_channel_egress_admission(20_000, ceiling, &mut slot, || {
        refused += 1;
        refused
    }) {
        ChannelEgressAdmission::Admitted {
            grew_to: Some(growth),
        } => {
            assert_eq!(growth.old_segment_bytes, 4096);
            assert_eq!(growth.new_segment_bytes, 32_768);
            assert!(
                !growth.crossed_quarter_ceiling,
                "new == ceiling/4 is not yet past the quarter — must not warn"
            );
        }
        other => panic!("expected an Admitted growth, got {other:?}"),
    }
    assert_eq!(slot, 32_768, "the slot advances to next_power_of_two");
    assert_eq!(refused, 0, "an admitted frame records no refusal");
}

#[test]
fn egress_admission_flags_the_growth_that_crosses_quarter_ceiling() {
    // Mental-revert guard for the quarter-ceiling early warning: this is the
    // single authority the host writer + Python/Deno natives all read the
    // `crossed_quarter_ceiling` flag from, so the threshold can't drift across
    // the three call sites. Drop the `> quarter && old <= quarter` computation
    // and this crossing goes unflagged — no runtime raises the warn.
    let ceiling = 128 * 1024usize; // quarter = 32 KiB = 32_768
    let mut refused = 0u64;
    let mut slot = 4096usize;
    // 40_000 → next_pow2 = 65_536, which is past the 32_768 quarter while the
    // old 4096 slot was under it: exactly the first crossing.
    match decide_channel_egress_admission(40_000, ceiling, &mut slot, || {
        refused += 1;
        refused
    }) {
        ChannelEgressAdmission::Admitted {
            grew_to: Some(growth),
        } => {
            assert_eq!(growth.old_segment_bytes, 4096);
            assert_eq!(growth.new_segment_bytes, 65_536);
            assert!(
                growth.crossed_quarter_ceiling,
                "old <= ceiling/4 < new must flag the quarter-ceiling crossing"
            );
        }
        other => panic!("expected an Admitted growth, got {other:?}"),
    }

    // A subsequent still-larger growth does NOT re-flag — the segment already
    // sits past the quarter, so only the FIRST crossing warns.
    match decide_channel_egress_admission(100_000, ceiling, &mut slot, || {
        refused += 1;
        refused
    }) {
        ChannelEgressAdmission::Admitted {
            grew_to: Some(growth),
        } => assert!(
            !growth.crossed_quarter_ceiling,
            "a growth already above the quarter must not re-flag"
        ),
        other => panic!("expected an Admitted growth, got {other:?}"),
    }
}

#[test]
fn egress_admission_admits_within_slot_without_growth() {
    let ceiling = 128 * 1024usize;
    let mut refused = 0u64;
    let mut slot = 65_536usize;
    // A frame at or under the tracked slot neither grows nor flags.
    assert_eq!(
        decide_channel_egress_admission(4096, ceiling, &mut slot, || {
            refused += 1;
            refused
        }),
        ChannelEgressAdmission::Admitted { grew_to: None }
    );
    assert_eq!(
        slot, 65_536,
        "an in-slot frame leaves the tracked slot as-is"
    );
    assert_eq!(refused, 0, "an admitted frame records no refusal");
}
