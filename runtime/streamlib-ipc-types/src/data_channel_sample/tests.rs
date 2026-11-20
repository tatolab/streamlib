// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

#[test]
fn the_channel_sample_user_header_sits_at_its_documented_offsets_under_its_pinned_name() {
    assert_eq!(size_of::<DataChannelSampleUserHeader>(), 96);
    assert_eq!(align_of::<DataChannelSampleUserHeader>(), 8);
    assert_eq!(
        [
            std::mem::offset_of!(DataChannelSampleUserHeader, sequence_number),
            std::mem::offset_of!(
                DataChannelSampleUserHeader,
                messages_lost_upstream_before_this_one
            ),
            std::mem::offset_of!(DataChannelSampleUserHeader, timestamp_ns),
            std::mem::offset_of!(DataChannelSampleUserHeader, payload_byte_len),
            std::mem::offset_of!(DataChannelSampleUserHeader, flags),
            std::mem::offset_of!(DataChannelSampleUserHeader, payload_descriptor),
        ],
        [0, 8, 16, 24, 28, 32]
    );
    assert_eq!(
        unsafe { <DataChannelSampleUserHeader as ZeroCopySend>::type_name() },
        "DataChannelSampleUserHeader",
    );
}

#[test]
fn the_payload_descriptor_is_a_kind_and_a_sixty_byte_body_under_its_pinned_name() {
    assert_eq!(size_of::<DataChannelPayloadDescriptor>(), 64);
    assert_eq!(align_of::<DataChannelPayloadDescriptor>(), 4);
    assert_eq!(std::mem::offset_of!(DataChannelPayloadDescriptor, kind), 0);
    assert_eq!(std::mem::offset_of!(DataChannelPayloadDescriptor, body), 4);
    assert_eq!(
        unsafe { <DataChannelPayloadDescriptor as ZeroCopySend>::type_name() },
        "DataChannelPayloadDescriptor",
    );
    assert_eq!(DataChannelPayloadDescriptorKind::Bytes as u32, 0);
    assert_eq!(DataChannelPayloadDescriptorKind::GpuPoolSlotFrame as u32, 1);
    assert_eq!(DataChannelPayloadDescriptor::bytes().body, [0u8; 60]);
}

#[test]
fn a_payload_cache_line_is_sixty_four_bytes_at_sixty_four_byte_alignment() {
    assert_eq!(size_of::<DataChannelPayloadCacheLine>(), 64);
    assert_eq!(align_of::<DataChannelPayloadCacheLine>(), 64);
    assert_eq!(
        unsafe { <DataChannelPayloadCacheLine as ZeroCopySend>::type_name() },
        "DataChannelPayloadCacheLine",
    );
    assert_eq!(data_channel_payload_cache_lines_for(0), 0);
    assert_eq!(data_channel_payload_cache_lines_for(1), 1);
    assert_eq!(data_channel_payload_cache_lines_for(64), 1);
    assert_eq!(data_channel_payload_cache_lines_for(65), 2);
}

#[test]
fn a_descriptor_of_an_unknown_kind_is_refused_by_kind() {
    let unknown = DataChannelPayloadDescriptor {
        kind: 8,
        body: [0u8; DATA_CHANNEL_PAYLOAD_DESCRIPTOR_BODY_BYTES],
    };
    assert_eq!(
        unknown.known_kind(),
        Err(DataChannelSamplePayloadRefusal::UnknownPayloadDescriptorKind { kind: 8 })
    );
}

#[test]
fn the_starts_group_flag_is_bit_zero_and_round_trips_through_the_header() {
    assert_eq!(DATA_CHANNEL_SAMPLE_FLAG_STARTS_GROUP, 0x0000_0001);
    for group_boundary in [
        MessageGroupBoundary::StartsGroup,
        MessageGroupBoundary::ContinuesGroup,
    ] {
        let header = DataChannelSampleUserHeader::new(
            -5,
            3,
            group_boundary,
            DataChannelPayloadDescriptor::bytes(),
        );
        assert_eq!(header.flags & !DATA_CHANNEL_SAMPLE_FLAG_STARTS_GROUP, 0);
        assert_eq!(header.group_boundary(), group_boundary);
    }
}

#[test]
fn a_header_bounds_its_payload_and_refuses_a_stamp_past_the_sample() {
    let header = DataChannelSampleUserHeader::new(
        0,
        3,
        MessageGroupBoundary::StartsGroup,
        DataChannelPayloadDescriptor::bytes(),
    );
    assert_eq!(header.payload_within(b"abcdef"), Ok(&b"abc"[..]));
    assert_eq!(
        header.payload_within(b"ab"),
        Err(
            DataChannelSamplePayloadRefusal::StampedLengthPastTheSample {
                stamped_payload_byte_len: 3,
                sample_payload_byte_len: 2,
            }
        )
    );
}
