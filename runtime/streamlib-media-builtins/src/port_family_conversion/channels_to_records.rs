// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Built-in `ChannelsToRecords`: each Channels block's latest value per
//! channel, as one Records message keyed by channel name.

use serde::{Deserialize, Serialize};
use streamlib::sdk::context::{RuntimeContextFullAccess, RuntimeContextLimitedAccess};
use streamlib::sdk::error::{Error, Result};
use streamlib::sdk::iceoryx2::{
    DataChannelPayloadDescriptor, RECORDS_MESSAGE_MAXIMUM_ENCODED_BYTES,
};
use streamlib::sdk::processors::ReactiveProcessor;
use streamlib::sdk::schemars::JsonSchema;

use super::conversion_node_config_defaults::one_channel_named_value;
use super::conversion_node_config_refusals::refuse_channel_names_naming_none_or_one_twice;
use super::port_family_messages_read_by_a_conversion::channels_block_read_by_a_conversion;

const CHANNELS_INPUT_PORT: &str = "channels";
const RECORDS_OUTPUT_PORT: &str = "records";

/// Configuration for [`ChannelsToRecords`].
#[streamlib::sdk::processor_config]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[schemars(crate = "streamlib::sdk::schemars")]
#[serde(deny_unknown_fields)]
pub struct ChannelsToRecordsConfig {
    #[serde(default = "one_channel_named_value")]
    #[description = "The key each channel's value is written under, in the block's channel order."]
    pub channel_names: Vec<String>,
}

impl Default for ChannelsToRecordsConfig {
    fn default() -> Self {
        Self {
            channel_names: one_channel_named_value(),
        }
    }
}

#[streamlib::sdk::processor(
    built_in_node,
    description = "Converts each Channels block on `channels` into one Records message on \
                  `records` holding the block's last sample of each channel as a float, keyed by \
                  that channel's name in `channel_names`: latest-value control, such as a \
                  sensor's reading or a fader's position, read by a node that takes Records. A \
                  block whose channel count is not the number of names is refused and writes \
                  nothing; a block of no samples writes nothing.",
    execution = reactive,
    config = crate::port_family_conversion::channels_to_records::ChannelsToRecordsConfig,
    input(
        "channels",
        description = "The Channels blocks whose latest values are read",
        delivery_profile = "newest",
        family = Channels
    ),
    output(
        "records",
        description = "One record per block: each channel's last sample under its name",
        family = Records,
        names = from_config("channel_names")
    ),
)]
pub struct ChannelsToRecords;

impl ReactiveProcessor for ChannelsToRecords::Processor {
    fn setup(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        refuse_channel_names_naming_none_or_one_twice(
            "ChannelsToRecords",
            &self.config.channel_names,
        )?;
        let encoded_byte_len =
            encoded_record_of_latest_values(&self.config.channel_names, |_| 0.0)?.len();
        if encoded_byte_len > RECORDS_MESSAGE_MAXIMUM_ENCODED_BYTES {
            return Err(Error::Configuration(format!(
                "ChannelsToRecords: a record of the {} names in `channel_names` encodes to \
                 {encoded_byte_len} bytes, past the {RECORDS_MESSAGE_MAXIMUM_ENCODED_BYTES} a \
                 Records message may take",
                self.config.channel_names.len()
            )));
        }
        Ok(())
    }

    fn process(&mut self, _ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        while let Some(message) = self.inputs.read_raw(CHANNELS_INPUT_PORT)? {
            let records_payload = match records_message_of_the_channels_block(
                &self.config,
                message.payload_descriptor,
                message.payload.bytes(),
            ) {
                Ok(Some(records_payload)) => records_payload,
                Ok(None) => continue,
                Err(refusal) => {
                    tracing::warn!(%refusal, "ChannelsToRecords wrote nothing for a message");
                    continue;
                }
            };
            self.outputs.write_bytes_described_by(
                RECORDS_OUTPUT_PORT,
                &records_payload,
                message.timestamp_ns,
                message.group_boundary,
                DataChannelPayloadDescriptor::records_message(),
            )?;
        }
        Ok(())
    }
}

/// The Records payload holding the last sample of each channel of the block
/// `payload_descriptor` and `payload` carry, or `None` for a block of no
/// samples.
pub fn records_message_of_the_channels_block(
    config: &ChannelsToRecordsConfig,
    payload_descriptor: DataChannelPayloadDescriptor,
    payload: &[u8],
) -> Result<Option<Vec<u8>>> {
    let block = channels_block_read_by_a_conversion(
        "ChannelsToRecords",
        CHANNELS_INPUT_PORT,
        payload_descriptor,
        payload,
    )?;
    if block.body.channel_count as usize != config.channel_names.len() {
        return Err(Error::PortError(format!(
            "ChannelsToRecords: a block on `channels` holds {} channels, and `channel_names` \
             names {}",
            block.body.channel_count,
            config.channel_names.len()
        )));
    }
    if block.body.samples_per_channel == 0 {
        return Ok(None);
    }
    encoded_record_of_latest_values(&config.channel_names, |channel_index| {
        block.last_sample_of_channel_as_f64(channel_index)
    })
    .map(Some)
}

/// The msgpack map holding, under each of `channel_names` in order, the value
/// `latest_value_of_channel` gives for its index.
fn encoded_record_of_latest_values(
    channel_names: &[String],
    latest_value_of_channel: impl Fn(usize) -> f64,
) -> Result<Vec<u8>> {
    let latest_value_by_channel_name = channel_names
        .iter()
        .enumerate()
        .map(|(channel_index, channel_name)| {
            (
                rmpv::Value::from(channel_name.as_str()),
                rmpv::Value::F64(latest_value_of_channel(channel_index)),
            )
        })
        .collect();
    let mut encoded = Vec::new();
    rmpv::encode::write_value(
        &mut encoded,
        &rmpv::Value::Map(latest_value_by_channel_name),
    )
    .map_err(|encode_failure| Error::BagEncodeFailed(encode_failure.to_string()))?;
    Ok(encoded)
}

#[cfg(test)]
mod tests {
    use streamlib::sdk::iceoryx2::{ChannelsBlockDescriptorBody, ChannelsBlockSampleType};

    use super::*;

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

        let encoded =
            records_message_of_the_channels_block(&rpm_and_torque(), descriptor, &payload)
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

        let refusal =
            records_message_of_the_channels_block(&rpm_and_torque(), descriptor, &payload)
                .unwrap_err()
                .to_string();

        assert!(refusal.contains("holds 1 channels"), "{refusal}");
    }
}
