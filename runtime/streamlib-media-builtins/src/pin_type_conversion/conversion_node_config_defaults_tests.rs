// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use streamlib::sdk::descriptors::NodeDescriptor;
use streamlib::sdk::nodes::GeneratedNode;

use super::conversion_node_config_refusals::refuse_a_channels_output_config;
use super::tensor_to_texture::image_format_named;
use super::{TensorToAudio, TensorToAudioInputs, TensorToTexture};

/// The default the value input `input` of `descriptor` declares.
fn the_default_of(descriptor: &NodeDescriptor, input: &str) -> serde_json::Value {
    descriptor
        .inputs
        .iter()
        .find(|port| port.name == input)
        .and_then(|port| port.pin_default.clone())
        .unwrap_or_else(|| panic!("`{input}` declares a default"))
}

/// Each conversion node holding the defaults its inputs declare builds what its frames read.
///
/// Mental-revert: a `channel_names` declared naming no channel, or a `format` no one tensor
/// holds, fails the first build of a node placed with no values set.
#[test]
fn each_conversion_node_given_no_values_holds_defaults_its_builds_accept() {
    let tensor_to_audio = <TensorToAudio::Node as GeneratedNode>::descriptor().unwrap();
    let held = TensorToAudioInputs {
        attribute: serde_json::from_value(the_default_of(&tensor_to_audio, "attribute")).unwrap(),
        channel_names: serde_json::from_value(the_default_of(&tensor_to_audio, "channel_names"))
            .unwrap(),
        sample_rate: serde_json::from_value(the_default_of(&tensor_to_audio, "sample_rate"))
            .unwrap(),
    };
    assert_eq!(held, TensorToAudioInputs::default());
    refuse_a_channels_output_config("TensorToAudio", &held.channel_names, held.sample_rate)
        .expect("TensorToAudio's defaults are accepted by its build");

    let tensor_to_texture = <TensorToTexture::Node as GeneratedNode>::descriptor().unwrap();
    let format = the_default_of(&tensor_to_texture, "format");
    image_format_named(format.as_str().expect("a format's wire name"))
        .expect("TensorToTexture's default is accepted by its build");
    assert_eq!(the_default_of(&tensor_to_texture, "attribute"), "image");
}
