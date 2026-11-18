// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use streamlib::sdk::iceoryx2::{
    ArrayFrameAttributeResidency, ArrayFrameDescriptorBody, ChannelsBlockDescriptorBody,
    ChannelsBlockSampleType, DataChannelPayloadDescriptor, array_frame_attribute_table_of,
};

use super::audio_to_tensor::*;

fn two_channel_f32_block_of_three_samples() -> (DataChannelPayloadDescriptor, Vec<u8>) {
    let samples: [f32; 6] = [0.5, 1.0, 1.5, -0.5, -1.0, -1.5];
    (
        DataChannelPayloadDescriptor::channels_block(ChannelsBlockDescriptorBody {
            start_sample: 960,
            sample_rate: 48_000,
            channel_count: 2,
            samples_per_channel: 3,
            sample_type: ChannelsBlockSampleType::F32 as u32,
        }),
        samples
            .iter()
            .flat_map(|sample| sample.to_le_bytes())
            .collect(),
    )
}

#[test]
fn a_channels_block_becomes_one_samples_attribute_of_channels_by_samples() {
    let (block_descriptor, block_payload) = two_channel_f32_block_of_three_samples();

    let (array_descriptor, array_payload) =
        array_frame_of_the_channels_block(block_descriptor, &block_payload).unwrap();

    let rows = array_frame_attribute_table_of(
        ArrayFrameDescriptorBody::from_descriptor_body(&array_descriptor.body),
        &array_payload,
    )
    .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].name, "samples");
    assert_eq!(rows[0].dtype, "float32");
    assert_eq!(rows[0].shape, vec![2, 3]);
    let ArrayFrameAttributeResidency::Cpu {
        payload_byte_offset,
        byte_len,
    } = rows[0].residency
    else {
        panic!("a CPU attribute");
    };
    assert_eq!(
        &array_payload[payload_byte_offset as usize..(payload_byte_offset + byte_len) as usize],
        &block_payload[..]
    );
}

#[test]
fn a_message_that_is_not_a_channels_block_is_refused_naming_its_kind() {
    let refusal =
        array_frame_of_the_channels_block(DataChannelPayloadDescriptor::records_message(), &[0x80])
            .unwrap_err()
            .to_string();

    assert!(refusal.contains("RecordsMessage"), "{refusal}");
    assert!(refusal.contains("ChannelsBlock"), "{refusal}");
}

#[test]
fn a_block_whose_payload_is_short_of_its_stated_samples_is_refused() {
    let (block_descriptor, block_payload) = two_channel_f32_block_of_three_samples();

    let refusal = array_frame_of_the_channels_block(block_descriptor, &block_payload[..20])
        .unwrap_err()
        .to_string();

    assert!(refusal.contains("20-byte payload"), "{refusal}");
}
