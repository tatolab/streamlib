// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::conversion_node_config_refusals::refuse_a_channels_output_config;
use super::tensor_to_texture::image_format_of_the_settings;
use super::{TensorToAudioSettings, TensorToTextureSettings};
use streamlib::sdk::descriptors::NodeSettings;

#[test]
fn each_conversion_node_given_no_config_takes_defaults_its_setup_accepts() {
    let tensor_to_audio =
        TensorToAudioSettings::read_from_the_input_values(&serde_json::Map::new())
            .expect("no values take the defaults");

    assert_eq!(tensor_to_audio, TensorToAudioSettings::default());
    refuse_a_channels_output_config(
        "TensorToAudio",
        &tensor_to_audio.channel_names,
        tensor_to_audio.sample_rate,
    )
    .expect("TensorToAudio's defaults are accepted at setup");

    let tensor_to_texture =
        TensorToTextureSettings::read_from_the_input_values(&serde_json::Map::new())
            .expect("no values take the defaults");
    assert_eq!(tensor_to_texture, TensorToTextureSettings::default());
    image_format_of_the_settings(&tensor_to_texture)
        .expect("TensorToTexture's defaults are accepted at setup");
}
