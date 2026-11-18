// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::records_to_channels::*;
use streamlib::sdk::iceoryx2::{
    ChannelsBlockDescriptorBody, ChannelsBlockSampleType, DataChannelPayloadDescriptor,
};

fn gain_and_mute() -> RecordsToChannelsConfig {
    RecordsToChannelsConfig {
        channel_names: vec!["gain".into(), "mute".into()],
        sample_rate: 30,
    }
}

fn records_payload_of(entries: Vec<(rmpv::Value, rmpv::Value)>) -> Vec<u8> {
    let mut encoded = Vec::new();
    rmpv::encode::write_value(&mut encoded, &rmpv::Value::Map(entries)).unwrap();
    encoded
}

fn samples_of(payload: &[u8]) -> Vec<f64> {
    payload
        .as_chunks::<8>()
        .0
        .iter()
        .map(|sample| f64::from_le_bytes(*sample))
        .collect()
}

#[test]
fn each_named_value_is_one_sample_of_its_channel() {
    let mut latest = vec![0.0; 2];
    let payload = records_payload_of(vec![
        ("mute".into(), rmpv::Value::Boolean(true)),
        ("gain".into(), rmpv::Value::F64(0.75)),
    ]);

    let (descriptor, samples) = channels_block_of_the_records_message(
        &gain_and_mute(),
        &mut latest,
        7,
        DataChannelPayloadDescriptor::records_message(),
        &payload,
    )
    .unwrap();

    assert_eq!(samples_of(&samples), vec![0.75, 1.0]);
    assert_eq!(
        ChannelsBlockDescriptorBody::from_descriptor_body(&descriptor.body),
        ChannelsBlockDescriptorBody {
            start_sample: 7,
            sample_rate: 30,
            channel_count: 2,
            samples_per_channel: 1,
            sample_type: ChannelsBlockSampleType::F64 as u32,
        }
    );
}

#[test]
fn a_key_the_record_leaves_out_keeps_its_latest_value_and_an_unknown_key_is_ignored() {
    let mut latest = vec![0.5, 1.0];
    let payload = records_payload_of(vec![
        ("gain".into(), rmpv::Value::from(2)),
        ("pan".into(), rmpv::Value::F64(-1.0)),
    ]);

    let (_, samples) = channels_block_of_the_records_message(
        &gain_and_mute(),
        &mut latest,
        0,
        DataChannelPayloadDescriptor::records_message(),
        &payload,
    )
    .unwrap();

    assert_eq!(samples_of(&samples), vec![2.0, 1.0]);
    assert_eq!(latest, vec![2.0, 1.0]);
}

#[test]
fn a_value_that_is_not_a_number_is_refused_and_changes_nothing() {
    let mut latest = vec![0.5, 0.0];
    let payload = records_payload_of(vec![
        ("mute".into(), rmpv::Value::F64(1.0)),
        ("gain".into(), rmpv::Value::from("loud")),
    ]);

    let refusal = channels_block_of_the_records_message(
        &gain_and_mute(),
        &mut latest,
        0,
        DataChannelPayloadDescriptor::records_message(),
        &payload,
    )
    .unwrap_err()
    .to_string();

    assert!(refusal.contains("\"gain\""), "{refusal}");
    assert_eq!(latest, vec![0.5, 0.0]);
}

#[test]
fn a_records_start_sample_is_its_stamp_since_the_first_at_the_sample_rate() {
    assert_eq!(start_sample_of_a_record_stamped(0, 30), 0);
    assert_eq!(start_sample_of_a_record_stamped(500_000_000, 30), 15);
    assert_eq!(start_sample_of_a_record_stamped(10_000_000_000, 30), 300);
    assert_eq!(start_sample_of_a_record_stamped(-1, 30), 0);
}

#[test]
fn a_message_that_is_not_one_map_is_refused() {
    let mut latest = vec![0.0; 2];
    let mut trailing = records_payload_of(vec![]);
    trailing.push(0xc0);
    let mut array = Vec::new();
    rmpv::encode::write_value(&mut array, &rmpv::Value::Array(vec![])).unwrap();

    for payload in [trailing, array] {
        assert!(
            channels_block_of_the_records_message(
                &gain_and_mute(),
                &mut latest,
                0,
                DataChannelPayloadDescriptor::records_message(),
                &payload,
            )
            .is_err()
        );
    }
}
