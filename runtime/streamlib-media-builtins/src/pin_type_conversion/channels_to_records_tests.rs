// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use streamlib::sdk::iceoryx2::{
    ChannelsBlockDescriptorBody, ChannelsBlockSampleType, DataChannelPayloadDescriptor,
};

use super::channels_to_records::*;

fn rpm_and_torque() -> ChannelsToRecordsConfig {
    ChannelsToRecordsConfig {
        channel_names: vec!["rpm".into(), "torque".into()],
    }
}

fn f32_block(channel_count: u32, samples: &[f32]) -> (DataChannelPayloadDescriptor, Vec<u8>) {
    (
        DataChannelPayloadDescriptor::channels_block(ChannelsBlockDescriptorBody {
            start_sample: 0,
            sample_rate: 100,
            channel_count,
            samples_per_channel: samples.len() as u32 / channel_count,
            sample_type: ChannelsBlockSampleType::F32 as u32,
        }),
        samples
            .iter()
            .flat_map(|sample| sample.to_le_bytes())
            .collect(),
    )
}

#[test]
fn each_channels_last_sample_is_written_under_its_name() {
    let (descriptor, payload) = f32_block(2, &[1000.0, 1500.0, 2000.0, 10.0, 12.5, 15.0]);

    let encoded = records_message_of_the_channels_block(&rpm_and_torque(), descriptor, &payload)
        .unwrap()
        .expect("a record");

    let record = rmpv::decode::read_value(&mut encoded.as_slice()).unwrap();
    assert_eq!(
        record,
        rmpv::Value::Map(vec![
            ("rpm".into(), rmpv::Value::F64(2000.0)),
            ("torque".into(), rmpv::Value::F64(15.0)),
        ])
    );
}

#[test]
fn a_block_of_no_samples_writes_nothing() {
    let (descriptor, payload) = f32_block(2, &[]);

    assert_eq!(
        records_message_of_the_channels_block(&rpm_and_torque(), descriptor, &payload).unwrap(),
        None
    );
}

#[test]
fn a_block_of_another_channel_count_is_refused() {
    let (descriptor, payload) = f32_block(1, &[1.0, 2.0]);

    let refusal = records_message_of_the_channels_block(&rpm_and_torque(), descriptor, &payload)
        .unwrap_err()
        .to_string();

    assert!(refusal.contains("holds 1 channels"), "{refusal}");
}
