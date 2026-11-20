// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use crate::{
    ChannelsBlockDescriptorBody, ChannelsBlockSampleType, GpuPoolSlotFrameDescriptorBody,
    ImageFrameDescriptorBody, ImageFrameH273Colour,
};

fn a_texture_descriptor() -> DataChannelPayloadDescriptor {
    DataChannelPayloadDescriptor::image_frame(ImageFrameDescriptorBody {
        slot: GpuPoolSlotFrameDescriptorBody {
            pool: [3u8; 16],
            slot: 5,
            generation: 11,
            timeline_value: 17,
            image_layout: 2,
        },
        width: 640,
        height: 360,
        format: 4,
        colour: ImageFrameH273Colour::UNSPECIFIED,
    })
}

fn an_audio_descriptor() -> DataChannelPayloadDescriptor {
    DataChannelPayloadDescriptor::channels_block(ChannelsBlockDescriptorBody {
        start_sample: 4800,
        sample_rate: 48_000,
        channel_count: 2,
        samples_per_channel: 2,
        sample_type: ChannelsBlockSampleType::F32 as u32,
    })
}

#[test]
fn a_bundle_frames_row_packs_every_field_at_its_documented_offset() {
    assert_eq!(BUNDLE_FRAME_ROW_BYTES, 128);
    assert_eq!(BUNDLE_FRAME_ROW_HOLDS_OFFSET, 0);
    assert_eq!(BUNDLE_FRAME_ROW_ELEMENT_INDEX_OFFSET, 4);
    assert_eq!(BUNDLE_FRAME_ROW_MEMBER_NAME_OFFSET, 8);
    assert_eq!(BUNDLE_FRAME_ROW_MEMBER_NAME_BYTES, 32);
    assert_eq!(BUNDLE_FRAME_ROW_DESCRIPTOR_KIND_OFFSET, 40);
    assert_eq!(BUNDLE_FRAME_ROW_DESCRIPTOR_BODY_OFFSET, 44);
    assert_eq!(BUNDLE_FRAME_ROW_PAYLOAD_OFFSET_OFFSET, 104);
    assert_eq!(BUNDLE_FRAME_ROW_PAYLOAD_BYTE_LEN_OFFSET, 112);
    assert_eq!(BUNDLE_FRAME_ROW_PAYLOAD_ALIGNMENT_BYTES, 64);
    assert_eq!(
        BUNDLE_FRAME_ROW_DESCRIPTOR_BODY_OFFSET + DATA_CHANNEL_PAYLOAD_DESCRIPTOR_BODY_BYTES,
        BUNDLE_FRAME_ROW_PAYLOAD_OFFSET_OFFSET
    );

    let picture = a_texture_descriptor();
    let row = BundleFrameRow {
        holds: BundleFrameRowHolds::Member("picture".to_owned()),
        descriptor: picture,
        payload_byte_offset: 0x0102_0304_0506_0708,
        payload_byte_len: 0x1112_1314_1516_1718,
    };
    let bytes = row.to_row_bytes().unwrap();
    assert_eq!(&bytes[0..4], &1u32.to_le_bytes());
    assert_eq!(&bytes[4..8], &[0u8; 4]);
    assert_eq!(&bytes[8..15], b"picture");
    assert!(bytes[15..40].iter().all(|&byte| byte == 0));
    assert_eq!(
        &bytes[40..44],
        &(DataChannelPayloadDescriptorKind::ImageFrame as u32).to_le_bytes()
    );
    assert_eq!(&bytes[44..104], &picture.body);
    assert_eq!(&bytes[104..112], &0x0102_0304_0506_0708u64.to_le_bytes());
    assert_eq!(&bytes[112..120], &0x1112_1314_1516_1718u64.to_le_bytes());
    assert_eq!(&bytes[120..128], &[0u8; 8]);
    assert_eq!(BundleFrameRow::from_row_bytes(&bytes).unwrap(), row);

    let element = BundleFrameRow {
        holds: BundleFrameRowHolds::Element(9),
        descriptor: picture,
        payload_byte_offset: 0,
        payload_byte_len: 0,
    };
    let element_bytes = element.to_row_bytes().unwrap();
    assert_eq!(&element_bytes[0..4], &2u32.to_le_bytes());
    assert_eq!(&element_bytes[4..8], &9u32.to_le_bytes());
    assert!(element_bytes[8..40].iter().all(|&byte| byte == 0));
    assert_eq!(
        BundleFrameRow::from_row_bytes(&element_bytes).unwrap(),
        element
    );
}

#[test]
fn a_bundle_frames_body_states_its_row_count_and_names_its_kind() {
    let body = BundleFrameDescriptorBody { row_count: 3 };
    let descriptor = DataChannelPayloadDescriptor::bundle_frame(body);
    assert_eq!(descriptor.kind, 7);
    assert_eq!(
        descriptor.known_kind(),
        Ok(DataChannelPayloadDescriptorKind::BundleFrame)
    );
    assert_eq!(
        &descriptor.body[..BUNDLE_FRAME_BODY_USED_BYTES],
        &3u32.to_le_bytes()
    );
    assert!(
        descriptor.body[BUNDLE_FRAME_BODY_USED_BYTES..]
            .iter()
            .all(|&byte| byte == 0)
    );
    assert_eq!(
        BundleFrameDescriptorBody::from_descriptor_body(&descriptor.body),
        body
    );
}

#[test]
fn a_packed_bundle_frame_reads_back_each_member_with_its_own_descriptor() {
    let small_members = [0x82u8, 0xa1, b'n', 0x01, 0xa1, b'm', 0xc3];
    let samples: Vec<u8> = (0u8..16).collect();
    let picture_holds = BundleFrameRowHolds::Member("picture".to_owned());
    let voice_holds = BundleFrameRowHolds::Member("voice".to_owned());
    let (descriptor, payload) = bundle_frame_of_rows(&[
        BundleFrameRowToPack {
            holds: &BundleFrameRowHolds::SmallMembers,
            descriptor: DataChannelPayloadDescriptor::records_message(),
            payload: &small_members,
        },
        BundleFrameRowToPack {
            holds: &picture_holds,
            descriptor: a_texture_descriptor(),
            payload: &[],
        },
        BundleFrameRowToPack {
            holds: &voice_holds,
            descriptor: an_audio_descriptor(),
            payload: &samples,
        },
    ])
    .unwrap();

    assert_eq!(
        descriptor.known_kind(),
        Ok(DataChannelPayloadDescriptorKind::BundleFrame)
    );
    let body = BundleFrameDescriptorBody::from_descriptor_body(&descriptor.body);
    let rows = bundle_frame_row_table_of(body, &payload).unwrap();
    let bytes_of = |row: &BundleFrameRow| {
        assert_eq!(
            row.payload_byte_offset % BUNDLE_FRAME_ROW_PAYLOAD_ALIGNMENT_BYTES as u64,
            0
        );
        &payload[row.payload_byte_offset as usize
            ..(row.payload_byte_offset + row.payload_byte_len) as usize]
    };
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].holds, BundleFrameRowHolds::SmallMembers);
    assert_eq!(
        rows[0].descriptor,
        DataChannelPayloadDescriptor::records_message()
    );
    assert_eq!(
        rows[0].payload_byte_offset,
        3 * BUNDLE_FRAME_ROW_BYTES as u64
    );
    assert_eq!(bytes_of(&rows[0]), &small_members[..]);
    assert_eq!(rows[1].holds, picture_holds);
    assert_eq!(rows[1].descriptor, a_texture_descriptor());
    assert_eq!(rows[1].payload_byte_len, 0);
    assert_eq!(rows[2].holds, voice_holds);
    assert_eq!(rows[2].descriptor, an_audio_descriptor());
    assert_eq!(bytes_of(&rows[2]), &samples[..]);
}

#[test]
fn a_list_of_textures_rides_one_bundle_frame_as_indexed_rows() {
    let first = BundleFrameRowHolds::Element(0);
    let second = BundleFrameRowHolds::Element(1);
    let (descriptor, payload) = bundle_frame_of_rows(&[
        BundleFrameRowToPack {
            holds: &first,
            descriptor: a_texture_descriptor(),
            payload: &[],
        },
        BundleFrameRowToPack {
            holds: &second,
            descriptor: a_texture_descriptor(),
            payload: &[],
        },
    ])
    .unwrap();
    let rows = bundle_frame_row_table_of(
        BundleFrameDescriptorBody::from_descriptor_body(&descriptor.body),
        &payload,
    )
    .unwrap();
    assert_eq!(
        rows.iter().map(|row| row.holds.clone()).collect::<Vec<_>>(),
        vec![first, second]
    );
}

#[test]
fn a_bundle_frame_refuses_a_row_that_rides_the_wrong_kind_or_holds_another_bundle_frame() {
    let picture = BundleFrameRowHolds::Member("picture".to_owned());
    let refusal_of = |holds: &BundleFrameRowHolds, descriptor| {
        bundle_frame_of_rows(&[BundleFrameRowToPack {
            holds,
            descriptor,
            payload: &[],
        }])
        .unwrap_err()
        .reason
    };
    assert!(
        refusal_of(&BundleFrameRowHolds::SmallMembers, a_texture_descriptor())
            .contains("ride a Records message")
    );
    assert!(
        refusal_of(&picture, DataChannelPayloadDescriptor::records_message())
            .contains("member `picture`")
    );
    assert!(
        refusal_of(
            &picture,
            DataChannelPayloadDescriptor::bundle_frame(BundleFrameDescriptorBody { row_count: 0 })
        )
        .contains("a bundle holds no bundle")
    );
    let unknown = DataChannelPayloadDescriptor {
        kind: 99,
        body: [0u8; DATA_CHANNEL_PAYLOAD_DESCRIPTOR_BODY_BYTES],
    };
    assert!(refusal_of(&picture, unknown).contains("kind 99"));
}

#[test]
fn a_bundle_frame_refuses_a_member_twice_and_members_mixed_with_elements() {
    let picture = BundleFrameRowHolds::Member("picture".to_owned());
    let element = BundleFrameRowHolds::Element(0);
    let row = |holds| BundleFrameRowToPack {
        holds,
        descriptor: a_texture_descriptor(),
        payload: &[],
    };
    assert!(
        bundle_frame_of_rows(&[row(&picture), row(&picture)])
            .unwrap_err()
            .reason
            .contains("member `picture` more than once")
    );
    assert!(
        bundle_frame_of_rows(&[row(&picture), row(&element)])
            .unwrap_err()
            .reason
            .contains("holds both")
    );
}

#[test]
fn a_bundle_frames_table_refuses_a_row_past_its_payload_or_inside_its_table() {
    let samples = [1u8; 8];
    let voice = BundleFrameRowHolds::Member("voice".to_owned());
    let (descriptor, payload) = bundle_frame_of_rows(&[BundleFrameRowToPack {
        holds: &voice,
        descriptor: an_audio_descriptor(),
        payload: &samples,
    }])
    .unwrap();
    let body = BundleFrameDescriptorBody::from_descriptor_body(&descriptor.body);

    assert!(
        bundle_frame_row_table_of(body, &payload[..BUNDLE_FRAME_ROW_BYTES + 4])
            .unwrap_err()
            .reason
            .contains("past the end")
    );
    assert!(
        bundle_frame_row_table_of(BundleFrameDescriptorBody { row_count: 2 }, &payload)
            .unwrap_err()
            .reason
            .contains("does not hold")
    );
    let mut into_the_table = payload.clone();
    into_the_table
        [BUNDLE_FRAME_ROW_PAYLOAD_OFFSET_OFFSET..BUNDLE_FRAME_ROW_PAYLOAD_OFFSET_OFFSET + 8]
        .copy_from_slice(&8u64.to_le_bytes());
    assert!(
        bundle_frame_row_table_of(body, &into_the_table)
            .unwrap_err()
            .reason
            .contains("inside the")
    );
}
