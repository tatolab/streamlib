// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Built-in `RecordsToChannels`: named values of each Records message as one
//! sample per channel, each channel holding its latest value.

use serde::{Deserialize, Serialize};
use streamlib::sdk::context::{RuntimeContextFullAccess, RuntimeContextLimitedAccess};
use streamlib::sdk::error::{Error, Result};
use streamlib::sdk::iceoryx2::{
    ChannelsBlockDescriptorBody, ChannelsBlockSampleType, DataChannelPayloadDescriptor,
    DataChannelPayloadDescriptorKind,
};
use streamlib::sdk::processors::ReactiveProcessor;
use streamlib::sdk::schemars::JsonSchema;

use super::conversion_node_config_refusals::refuse_a_channels_output_config;
use super::port_family_messages_read_by_a_conversion::refuse_unless_the_descriptor_is;

const RECORDS_INPUT_PORT: &str = "records";
const CHANNELS_OUTPUT_PORT: &str = "channels";

/// Configuration for [`RecordsToChannels`].
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[schemars(crate = "streamlib::sdk::schemars")]
#[serde(deny_unknown_fields)]
pub struct RecordsToChannelsConfig {
    /// The record keys read, one channel each, in channel order.
    pub channel_names: Vec<String>,
    /// The sample rate `channels` states, in samples per second per channel.
    pub sample_rate: u32,
}

#[streamlib::sdk::processor(
    built_in_node,
    description = "Converts each Records message on `records` into a Channels block of one \
                  `f64` sample per channel on `channels`, one channel per key in \
                  `channel_names`: latest-value control, such as a parameter an agent or a UI \
                  sets, read by a node that takes Channels. A key the record leaves out keeps \
                  its channel's latest value, 0 until a record names it; a key not in \
                  `channel_names` is ignored. A value is an integer or a float, or a boolean \
                  read as 1 or 0; a record that is not a map of string keys, or that holds \
                  another value under a name, is refused and writes nothing. Each block's start \
                  sample is the record's stamp since the first record's, counted at \
                  `sample_rate`, and always past the block before it.",
    execution = reactive,
    config = crate::port_family_conversion::records_to_channels::RecordsToChannelsConfig,
    input(
        "records",
        description = "The records whose named values are read",
        delivery_profile = "newest",
        family = Records
    ),
    output(
        "channels",
        description = "One sample per record of each channel in `channel_names`",
        family = Channels,
        names = from_config("channel_names"),
        rate = from_config("sample_rate"),
        dtype = "f64"
    ),
)]
pub struct RecordsToChannels {
    latest_value_of_each_channel: Vec<f64>,
    first_record_timestamp_ns: Option<i64>,
    next_start_sample: u64,
}

impl ReactiveProcessor for RecordsToChannels::Processor {
    fn setup(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        refuse_a_channels_output_config(
            "RecordsToChannels",
            &self.config.channel_names,
            self.config.sample_rate,
        )?;
        self.latest_value_of_each_channel = vec![0.0; self.config.channel_names.len()];
        Ok(())
    }

    fn process(&mut self, _ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        while let Some(message) = self.inputs.read_raw(RECORDS_INPUT_PORT)? {
            let first_record_timestamp_ns = *self
                .first_record_timestamp_ns
                .get_or_insert(message.timestamp_ns);
            let start_sample = start_sample_of_a_record_stamped(
                message.timestamp_ns - first_record_timestamp_ns,
                self.config.sample_rate,
            )
            .max(self.next_start_sample);
            let (payload_descriptor, payload) = match channels_block_of_the_records_message(
                &self.config,
                &mut self.latest_value_of_each_channel,
                start_sample,
                message.payload_descriptor,
                message.payload.bytes(),
            ) {
                Ok(block) => block,
                Err(refusal) => {
                    tracing::warn!(%refusal, "RecordsToChannels wrote nothing for a message");
                    continue;
                }
            };
            self.next_start_sample = start_sample + 1;
            self.outputs.write_bytes_described_by(
                CHANNELS_OUTPUT_PORT,
                &payload,
                message.timestamp_ns,
                message.group_boundary,
                payload_descriptor,
            )?;
        }
        Ok(())
    }
}

/// The sample index, at `sample_rate`, of a record stamped
/// `nanoseconds_since_the_first_record` after the first; 0 for one stamped
/// before it.
pub fn start_sample_of_a_record_stamped(
    nanoseconds_since_the_first_record: i64,
    sample_rate: u32,
) -> u64 {
    let nanoseconds = u128::try_from(nanoseconds_since_the_first_record).unwrap_or_default();
    u64::try_from(nanoseconds * u128::from(sample_rate) / 1_000_000_000).unwrap_or(u64::MAX)
}

/// The one-sample Channels block holding, per name in `config`, the value
/// the Records message `payload_descriptor` and `payload` carry, or the
/// channel's value in `latest_value_of_each_channel` where it names none;
/// `latest_value_of_each_channel` is updated to the block's.
pub fn channels_block_of_the_records_message(
    config: &RecordsToChannelsConfig,
    latest_value_of_each_channel: &mut [f64],
    start_sample: u64,
    payload_descriptor: DataChannelPayloadDescriptor,
    payload: &[u8],
) -> Result<(DataChannelPayloadDescriptor, Vec<u8>)> {
    refuse_unless_the_descriptor_is(
        "RecordsToChannels",
        RECORDS_INPUT_PORT,
        payload_descriptor,
        DataChannelPayloadDescriptorKind::RecordsMessage,
    )?;
    let mut remaining_payload = payload;
    let record = rmpv::decode::read_value(&mut remaining_payload).map_err(|decode_failure| {
        Error::BagDecodeFailed(format!(
            "RecordsToChannels: a message on `records` is not msgpack: {decode_failure}"
        ))
    })?;
    if !remaining_payload.is_empty() {
        return Err(Error::BagDecodeFailed(format!(
            "RecordsToChannels: a message on `records` holds {} bytes past its record",
            remaining_payload.len()
        )));
    }
    let rmpv::Value::Map(entries) = record else {
        return Err(Error::PortError(
            "RecordsToChannels: a message on `records` is not a map".into(),
        ));
    };
    let mut values_this_record_names = latest_value_of_each_channel.to_vec();
    for (key, value) in &entries {
        let Some(key) = key.as_str() else {
            return Err(Error::PortError(format!(
                "RecordsToChannels: a record on `records` has the key {key}, which is not a string"
            )));
        };
        let Some(channel_index) = config.channel_names.iter().position(|name| name == key) else {
            continue;
        };
        values_this_record_names[channel_index] = match value {
            rmpv::Value::Integer(integer) => integer.as_f64().unwrap_or_default(),
            rmpv::Value::F32(float) => f64::from(*float),
            rmpv::Value::F64(float) => *float,
            rmpv::Value::Boolean(flag) => f64::from(u8::from(*flag)),
            other => {
                return Err(Error::PortError(format!(
                    "RecordsToChannels: a record on `records` holds {other} under {key:?}, \
                     which is not a number"
                )));
            }
        };
    }
    latest_value_of_each_channel.copy_from_slice(&values_this_record_names);
    let payload = values_this_record_names
        .iter()
        .flat_map(|value| value.to_le_bytes())
        .collect();
    Ok((
        DataChannelPayloadDescriptor::channels_block(ChannelsBlockDescriptorBody {
            start_sample,
            sample_rate: config.sample_rate,
            channel_count: config.channel_names.len() as u32,
            samples_per_channel: 1,
            sample_type: ChannelsBlockSampleType::F64 as u32,
        }),
        payload,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
