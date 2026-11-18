// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::conversion_node_config_refusals::{
    refuse_a_channels_output_config, refuse_channel_names_naming_none_or_one_twice,
};
use super::tensor_to_texture::image_format_of_the_config;
use super::{
    ChannelsToRecordsConfig, RecordsToChannelsConfig, TensorToAudioConfig, TensorToTextureConfig,
};

#[test]
fn each_conversion_node_given_no_config_takes_defaults_its_setup_accepts() {
    let tensor_to_audio: TensorToAudioConfig =
        serde_json::from_str("{}").expect("an empty config takes the defaults");
    let records_to_channels: RecordsToChannelsConfig =
        serde_json::from_str("{}").expect("an empty config takes the defaults");
    let channels_to_records: ChannelsToRecordsConfig =
        serde_json::from_str("{}").expect("an empty config takes the defaults");

    assert_eq!(tensor_to_audio, TensorToAudioConfig::default());
    assert_eq!(records_to_channels, RecordsToChannelsConfig::default());
    assert_eq!(channels_to_records, ChannelsToRecordsConfig::default());
    refuse_a_channels_output_config(
        "TensorToAudio",
        &tensor_to_audio.channel_names,
        tensor_to_audio.sample_rate,
    )
    .expect("TensorToAudio's defaults are accepted at setup");
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

    let tensor_to_texture: TensorToTextureConfig =
        serde_json::from_str("{}").expect("an empty config takes the defaults");
    assert_eq!(tensor_to_texture, TensorToTextureConfig::default());
    image_format_of_the_config(&tensor_to_texture)
        .expect("TensorToTexture's defaults are accepted at setup");
}
