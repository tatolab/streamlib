// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The config defaults the conversion nodes share, so a node placed with no
//! config takes its config and runs.

/// The one channel a conversion node names when its config names none.
pub(crate) fn one_channel_named_value() -> Vec<String> {
    vec!["value".to_string()]
}

/// The sample rate a conversion node's Channels output states when its config states none.
pub(crate) fn forty_eight_thousand_samples_per_second() -> u32 {
    48_000
}

#[cfg(test)]
mod tests {
    use super::super::array_to_image::image_format_of_the_config;
    use super::super::conversion_node_config_refusals::{
        refuse_a_channels_output_config, refuse_channel_names_naming_none_or_one_twice,
    };
    use super::super::{
        ArrayToChannelsConfig, ArrayToImageConfig, ChannelsToRecordsConfig, RecordsToChannelsConfig,
    };

    /// Each conversion node's `#[processor_config]` `Default` is a config its
    /// node's setup accepts.
    #[test]
    fn each_conversion_node_given_no_config_takes_defaults_its_setup_accepts() {
        let array_to_channels: ArrayToChannelsConfig =
            serde_json::from_str("{}").expect("an empty config takes the defaults");
        let records_to_channels: RecordsToChannelsConfig =
            serde_json::from_str("{}").expect("an empty config takes the defaults");
        let channels_to_records: ChannelsToRecordsConfig =
            serde_json::from_str("{}").expect("an empty config takes the defaults");

        assert_eq!(array_to_channels, ArrayToChannelsConfig::default());
        assert_eq!(records_to_channels, RecordsToChannelsConfig::default());
        assert_eq!(channels_to_records, ChannelsToRecordsConfig::default());
        refuse_a_channels_output_config(
            "ArrayToChannels",
            &array_to_channels.channel_names,
            array_to_channels.sample_rate,
        )
        .expect("ArrayToChannels's defaults are accepted at setup");
        refuse_a_channels_output_config(
            "RecordsToChannels",
            &records_to_channels.channel_names,
            records_to_channels.sample_rate,
        )
        .expect("RecordsToChannels's defaults are accepted at setup");
        refuse_channel_names_naming_none_or_one_twice(
            "ChannelsToRecords",
            &channels_to_records.channel_names,
        )
        .expect("ChannelsToRecords's defaults are accepted at setup");

        let array_to_image: ArrayToImageConfig =
            serde_json::from_str("{}").expect("an empty config takes the defaults");
        assert_eq!(array_to_image, ArrayToImageConfig::default());
        image_format_of_the_config(&array_to_image)
            .expect("ArrayToImage's defaults are accepted at setup");
    }
}
