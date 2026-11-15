// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use crate::*;

/// The shape of `MessageTypeDetails::chunk_layout`, spelled out here so a
/// one-sided edit of the constants reddens. Both sides read the same upstream
/// types, so this deliberately does NOT catch an upstream size change — that
/// moves the constants and the formula together, which is the point of
/// composing them from the types rather than writing a number. What catches
/// an upstream change to the formula itself is the engine's
/// `a_ceiling_sized_bag_fills_the_ceilings_chunk_and_one_byte_more_takes_the_next`,
/// which loans a real chunk.
#[test]
fn a_channel_samples_layout_is_iceoryx2s_own_over_the_two_headers_and_the_payload() {
    type Iceoryx2PublishSubscribeHeader = iceoryx2::service::header::publish_subscribe::Header;
    type Line = DataChannelPayloadCacheLine;
    let all_headers_len = (size_of::<Iceoryx2PublishSubscribeHeader>()
        .next_multiple_of(align_of::<DataChannelSampleUserHeader>())
        + size_of::<DataChannelSampleUserHeader>())
    .next_multiple_of(align_of::<Line>());
    let max_alignment = align_of::<Iceoryx2PublishSubscribeHeader>()
        .max(align_of::<DataChannelSampleUserHeader>())
        .max(align_of::<Line>());
    assert_eq!(
        all_headers_len % 64,
        0,
        "the payload must start 64-byte aligned"
    );
    for frame_total_bytes in [0usize, 1, 7, 8, 9, 64, 65, 4096, 65_536, 1_000_003] {
        let upstream_formula = (all_headers_len
            + size_of::<Line>().next_multiple_of(align_of::<Line>())
                * frame_total_bytes.div_ceil(size_of::<Line>()))
        .next_multiple_of(max_alignment);
        assert_eq!(
            iceoryx2_sample_bytes_for_a_channel_frame(frame_total_bytes),
            upstream_formula,
            "the sample size for a {frame_total_bytes}-byte frame must be iceoryx2's own"
        );
    }
}

/// The ceiling arithmetic is exact for any ceiling an operator's per-tier
/// override can set, not only for the two powers of two the tiers default
/// to: the admitted frame's sample fits and the next byte's does not.
#[test]
fn the_admitted_frame_is_exact_at_every_ceiling_an_override_can_set() {
    for chunk_ceiling_bytes in [
        1usize,
        55,
        56,
        57,
        64,
        100,
        4095,
        4096,
        100_003,
        TRUSTED_CHANNEL_CHUNK_CEILING_BYTES,
        TRUSTED_CHANNEL_CHUNK_CEILING_BYTES + 1,
    ] {
        let largest_admitted_frame_bytes =
            largest_channel_frame_bytes_under_a_chunk_ceiling(chunk_ceiling_bytes);
        if largest_admitted_frame_bytes == 0 {
            // The degenerate arm: a ceiling under the sample headers and
            // one payload line admits no byte of payload.
            assert!(iceoryx2_sample_bytes_for_a_channel_frame(1) > chunk_ceiling_bytes);
            continue;
        }
        assert!(
            iceoryx2_sample_bytes_for_a_channel_frame(largest_admitted_frame_bytes)
                <= chunk_ceiling_bytes,
            "the admitted frame's sample must fit a {chunk_ceiling_bytes}-byte ceiling"
        );
        assert!(
            iceoryx2_sample_bytes_for_a_channel_frame(largest_admitted_frame_bytes + 1)
                > chunk_ceiling_bytes,
            "one byte more must not fit a {chunk_ceiling_bytes}-byte ceiling"
        );
    }
}

/// Mental-revert guard for the ceiling half of the shared-memory accounting:
/// compare the FRAME against the ceiling instead of its sample, and a
/// ceiling-sized bag is admitted into a chunk twice the ceiling — the
/// container `/dev/shm` SIGBUS this subtraction exists to keep away from.
#[test]
fn a_frame_at_the_admitted_ceiling_keeps_its_sample_inside_one_ceiling_sized_chunk() {
    for chunk_ceiling_bytes in [
        TRUSTED_CHANNEL_CHUNK_CEILING_BYTES,
        UNTRUSTED_SESSION_CHANNEL_CHUNK_CEILING_BYTES,
    ] {
        let largest_admitted_frame_bytes =
            largest_channel_frame_bytes_under_a_chunk_ceiling(chunk_ceiling_bytes);
        assert!(
            largest_admitted_frame_bytes < chunk_ceiling_bytes,
            "the admitted frame must leave room for iceoryx2's sample headers"
        );
        assert_eq!(
            chunk_bytes_a_ceiling_sized_bag_takes(chunk_ceiling_bytes),
            Some(chunk_ceiling_bytes),
            "the largest admitted frame must bucket at exactly the ceiling"
        );
        assert_eq!(
            iceoryx2_sample_bytes_for_a_channel_frame(chunk_ceiling_bytes).next_power_of_two(),
            chunk_ceiling_bytes * 2,
            "a frame sized to the ceiling itself is what takes a double-sized chunk"
        );
    }
}

/// The composite never panics on a ceiling an operator's env override can
/// set, however absurd — it is read from inside a warning, and a diagnostic
/// that kills the engine is worse than the misconfiguration it describes.
#[test]
fn the_chunk_a_ceiling_sized_bag_takes_is_unknown_rather_than_a_panic_at_the_extremes() {
    assert_eq!(chunk_bytes_a_ceiling_sized_bag_takes(usize::MAX), None);
    assert_eq!(
        chunk_bytes_a_ceiling_sized_bag_takes(TRUSTED_CHANNEL_CHUNK_CEILING_BYTES),
        Some(TRUSTED_CHANNEL_CHUNK_CEILING_BYTES)
    );
    // A ceiling under the sample headers admits no frame at all; the chunk an
    // empty one would take is still a real, small number.
    assert!(chunk_bytes_a_ceiling_sized_bag_takes(32).is_some());
}

#[test]
fn a_frame_one_byte_past_the_admitted_ceiling_is_refused_and_the_admitted_one_is_not() {
    let chunk_ceiling_bytes = TRUSTED_CHANNEL_CHUNK_CEILING_BYTES;
    let largest_admitted_frame_bytes =
        largest_channel_frame_bytes_under_a_chunk_ceiling(chunk_ceiling_bytes);
    let mut slot = DEFAULT_EXPECTED_PAYLOAD_BYTES;

    assert!(
        matches!(
            decide_channel_egress_admission(
                largest_admitted_frame_bytes,
                chunk_ceiling_bytes,
                &mut slot,
                || panic!("the largest admitted frame must not be refused"),
            ),
            ChannelEgressAdmission::Admitted { .. }
        ),
        "the largest frame whose sample fits the chunk must be admitted"
    );
    assert_eq!(
        slot, chunk_ceiling_bytes,
        "the tracked slot is the sample's bucket, which is the ceiling itself"
    );

    assert_eq!(
        decide_channel_egress_admission(
            largest_admitted_frame_bytes + 1,
            chunk_ceiling_bytes,
            &mut slot,
            || 1,
        ),
        ChannelEgressAdmission::RefusedOverCeiling { refused_count: 1 },
        "one byte more would need a chunk twice the ceiling"
    );
}
