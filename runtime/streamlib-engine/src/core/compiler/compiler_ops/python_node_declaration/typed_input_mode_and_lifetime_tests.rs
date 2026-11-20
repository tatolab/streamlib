// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! How a typed input's author states it takes its value, and how long the value lasts, read
//! off a described port.

use super::tests::{described_node_type_with_input_ports, read, refusal_of};
use super::*;

/// A class node's typed field `quality`, a setting read at setup forgotten at Stop, and its
/// typed field `wet`, which takes only a wire.
fn a_described_class_node_with_typed_fields() -> serde_json::Value {
    described_node_type_with_input_ports(serde_json::json!([
        {
            "name": "quality", "description": "", "family": {"family": "records"},
            "pin_declaration": {
                "pin_type": {"kind": "is", "pin_type": {"type": "menu", "option_names": ["fast", "fine"]}},
                "flow": "value",
            },
            "pin_default": "fast",
            "input_mode": {"mode": "setting_only", "read": "at_setup"},
            "lifetime": "reset",
        },
        {
            "name": "wet", "description": "", "family": {"family": "channels"},
            "pin_declaration": {"pin_type": {"kind": "is", "pin_type": {"type": "float"}}, "flow": "value"},
            "pin_default": 1.0,
            "input_mode": {"mode": "wire_only"},
        },
    ]))
}

#[test]
fn a_class_node_fields_mode_and_lifetime_reach_its_descriptor() {
    let declaration = read(&a_described_class_node_with_typed_fields()).expect("reads");
    let input = |name: &str| {
        declaration
            .descriptor
            .inputs
            .iter()
            .find(|port| port.name == name)
            .expect("the field is an input")
    };

    assert_eq!(
        (input("quality").input_mode, input("quality").lifetime),
        (
            Some(NodeInputMode::SettingOnly {
                read: crate::core::descriptors::SettingReadTime::AtSetup
            }),
            NodeValueLifetime::Reset
        )
    );
    assert_eq!(
        (input("wet").input_mode, input("wet").lifetime),
        (Some(NodeInputMode::WireOnly), NodeValueLifetime::Kept)
    );
}

#[test]
fn a_mode_or_lifetime_off_a_typed_input_is_refused_naming_the_port() {
    let mut mode_on_an_untyped_input = a_described_class_node_with_typed_fields();
    mode_on_an_untyped_input["input_ports"][1]
        .as_object_mut()
        .expect("a port")
        .remove("pin_declaration");
    mode_on_an_untyped_input["input_ports"][1]["delivery_profile"] = serde_json::json!("newest");
    let mut lifetime_on_an_output = a_described_class_node_with_typed_fields();
    lifetime_on_an_output["output_ports"][0]["lifetime"] = serde_json::json!("kept");
    let mut unknown_mode = a_described_class_node_with_typed_fields();
    unknown_mode["input_ports"][0]["input_mode"] = serde_json::json!({"mode": "sometimes"});

    assert!(
        refusal_of(&mode_on_an_untyped_input)
            .contains("input port \"wet\" declares an input_mode and no pin type")
    );
    assert!(refusal_of(&lifetime_on_an_output).contains("port \"levels\" declares a lifetime"));
    assert!(refusal_of(&unknown_mode).contains("input port \"quality\": input_mode"));
}
