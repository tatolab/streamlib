// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use streamlib::sdk::json_schema::NodeDescriptorOutput;
use streamlib::sdk::nodes::GeneratedNode;
use streamlib::sdk::serde_json::{self, Map, Value, json};
use streamlib_test_fixtures::test_configured_node::{ConfiguredNode, ConfiguredNodeSettings};

fn held(values: Value) -> Map<String, Value> {
    values.as_object().cloned().expect("an object of values")
}

/// The catalog lists `threshold` as the fixture's one input, a Float setting read at setup
/// holding its default, and no config schema.
///
/// Mental-revert: a fixture declaring `config =` lists no input and states a config schema.
#[test]
fn the_catalog_lists_its_setting_as_an_input_read_at_setup() {
    let descriptor = <ConfiguredNode::Node as GeneratedNode>::descriptor().expect("a descriptor");
    let rendered = serde_json::to_value(NodeDescriptorOutput::from(&descriptor)).unwrap();
    let inputs = rendered["inputs"].as_array().unwrap();
    assert_eq!(inputs.len(), 1, "{rendered}");
    let threshold = &inputs[0];
    assert_eq!(threshold["name"], "threshold", "{rendered}");
    assert_eq!(
        threshold["pin_type"]["pin_type"],
        json!({"type": "float"}),
        "{rendered}"
    );
    assert_eq!(threshold["default_value"], json!(0.0), "{rendered}");
    assert_eq!(
        threshold["input_mode"],
        json!({"mode": "setting_only", "read": "at_setup"})
    );
    assert!(descriptor.config_schema.is_none(), "{rendered}");
}

/// The settings the input values hold before setup are the ones the node holds; a value its
/// setting does not take is refused, naming the node.
///
/// Mental-revert: settings left at their defaults hold `0.0` after the input holds `0.5`.
#[test]
fn the_settings_held_before_setup_are_the_ones_it_holds() {
    let mut node = ConfiguredNode::Node::from_config(Default::default()).unwrap();
    assert_eq!(node.settings, ConfiguredNodeSettings::default());

    node.set_unlinked_input_values(&held(json!({"threshold": 0.5})))
        .expect("the settings are taken");
    assert_eq!(node.settings.threshold, 0.5);

    let refusal = node
        .set_unlinked_input_values(&held(json!({"threshold": "high"})))
        .expect_err("a string is no Float");
    assert!(refusal.to_string().contains("ConfiguredNode"), "{refusal}");
}

/// A spec of the node built from settings holds them as its input values, under each key the
/// config held, and an empty config.
///
/// Mental-revert: a spec built from a config holds `threshold` in its config.
#[test]
fn a_spec_built_from_settings_holds_them_as_input_values() {
    let spec = ConfiguredNode::node(ConfiguredNodeSettings { threshold: 0.8 });
    assert_eq!(spec.pins, held(json!({"threshold": 0.8_f32})));
    assert_eq!(spec.config, json!({}));
}
