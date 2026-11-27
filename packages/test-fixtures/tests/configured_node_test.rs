// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use streamlib::sdk::json_schema::NodeDescriptorOutput;
use streamlib::sdk::nodes::GeneratedNode;
use streamlib::sdk::serde_json::{self, Map, Value, json};
use streamlib_test_fixtures::test_configured_node::ConfiguredNode;

fn held(values: Value) -> Map<String, Value> {
    values.as_object().cloned().expect("an object of values")
}

/// The catalog lists `threshold` as the fixture's one input, a Float value input read each run
/// holding its default.
///
/// Mental-revert: a value input listed as read at setup would be held on the graph and rebuild
/// the node when set.
#[test]
fn the_catalog_lists_its_value_input_as_an_input_read_each_run() {
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
        json!({"mode": "setting_only", "read": "each_run"}),
        "{rendered}"
    );
}

/// A value its input takes is taken before setup; a value it does not take is refused, naming
/// the node.
///
/// Mental-revert: a value input that took any JSON would take `"high"` as a Float.
#[test]
fn a_value_its_input_does_not_take_is_refused_naming_the_node() {
    let mut node = ConfiguredNode::Node::from_config(Default::default()).unwrap();

    node.set_unlinked_input_values(&held(json!({"threshold": 0.5})))
        .expect("a Float is taken");

    let refusal = node
        .set_unlinked_input_values(&held(json!({"threshold": "high"})))
        .expect_err("a string is no Float");
    assert!(refusal.to_string().contains("ConfiguredNode"), "{refusal}");
}
