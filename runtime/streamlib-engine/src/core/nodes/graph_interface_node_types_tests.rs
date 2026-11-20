// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::graph_interface_node_types::{
    the_node_type_of_a_graph_input, the_node_type_of_a_graph_output,
};
use crate::core::descriptors::{
    DeclaredPinType, NodeDescriptor, NodeInputMode, PinType, SettingReadTime,
};

fn the_setting_names(descriptor: &NodeDescriptor) -> Vec<&str> {
    descriptor
        .inputs
        .iter()
        .filter(|input| {
            input.input_mode
                == Some(NodeInputMode::SettingOnly {
                    read: SettingReadTime::AtSetup,
                })
        })
        .map(|input| input.name.as_str())
        .collect()
}

fn the_declared_type_of(descriptor: &NodeDescriptor, input: &str) -> DeclaredPinType {
    descriptor
        .inputs
        .iter()
        .find(|port| port.name == input)
        .and_then(|port| port.declared_pin())
        .unwrap_or_else(|| panic!("`{input}` declares a pin"))
        .pin_type
}

/// Mental-revert: declaring a Graph Input's settings as config keys, as before, lists none of
/// them as an input, and a `value` that keeps declaring any type names no setting choosing it.
#[test]
fn a_graph_input_declares_each_setting_as_an_input_and_its_value_of_the_type_pin_type_chooses() {
    let descriptor = the_node_type_of_a_graph_input().expect("the GraphInput type builds");
    assert_eq!(
        the_setting_names(&descriptor),
        ["port", "pin_type", "family", "range", "lifetime"]
    );
    assert_eq!(
        the_declared_type_of(&descriptor, "pin_type"),
        DeclaredPinType::Is {
            pin_type: PinType::PinTypeChoice { among: Vec::new() }
        }
    );
    let value = descriptor
        .inputs
        .iter()
        .find(|port| port.name == "value")
        .expect("a GraphInput has a `value` input");
    assert_eq!(
        value.input_mode,
        Some(NodeInputMode::SettingOnly {
            read: SettingReadTime::EachRun
        })
    );
    let value_declared = value.declared_pin().expect("`value` declares a pin");
    assert_eq!(value_declared.former_names, ["default"]);
    assert!(
        value_declared.may_be_unset,
        "a client clears the value, as the Python class's `object | None` says"
    );
    assert!(
        !descriptor.outputs[0]
            .declared_pin()
            .expect("the `value` output declares a pin")
            .may_be_unset
    );
    for value in [value_declared.pin_type, the_output_value_type(&descriptor)] {
        assert_eq!(
            value,
            DeclaredPinType::SetByInput {
                input_name: "pin_type".to_string()
            }
        );
    }
    assert_eq!(
        descriptor.refusal_of_a_port_whose_type_no_input_finds(),
        None
    );
}

fn the_output_value_type(descriptor: &NodeDescriptor) -> DeclaredPinType {
    descriptor
        .outputs
        .iter()
        .find(|port| port.name == "value")
        .and_then(|port| port.declared_pin())
        .expect("a GraphInput's `value` output declares a pin")
        .pin_type
}

/// Mental-revert: a GraphOutput declaring no settings lists only its `value`.
#[test]
fn a_graph_output_declares_its_settings_and_a_value_that_takes_a_wire() {
    let descriptor = the_node_type_of_a_graph_output().expect("the GraphOutput type builds");
    assert_eq!(
        the_setting_names(&descriptor),
        ["port", "pin_type", "family"]
    );
    let value = descriptor
        .inputs
        .iter()
        .find(|port| port.name == "value")
        .expect("a GraphOutput has a `value` input");
    assert!(value.input_mode.is_none_or(NodeInputMode::takes_a_wire));
    assert!(descriptor.outputs.is_empty());
    assert_eq!(
        descriptor.refusal_of_a_port_whose_type_no_input_finds(),
        None
    );
}

/// Mental-revert: a `lifetime` declared with no default reads nothing on a Graph Input that
/// saves none.
#[test]
fn a_graph_inputs_lifetime_is_a_menu_whose_default_is_kept() {
    let descriptor = the_node_type_of_a_graph_input().expect("the GraphInput type builds");
    let lifetime = descriptor
        .inputs
        .iter()
        .find(|port| port.name == "lifetime")
        .expect("a GraphInput has a `lifetime` setting");
    assert_eq!(lifetime.pin_default, Some(serde_json::json!("kept")));
    assert_eq!(
        the_declared_type_of(&descriptor, "lifetime"),
        DeclaredPinType::Is {
            pin_type: PinType::Menu {
                option_names: vec!["kept".to_string(), "reset".to_string()]
            }
        }
    );
}
