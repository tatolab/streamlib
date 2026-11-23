// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The pin groups a described class node declares, each slot's pins read as the node's own
//! ports are.

use super::tests::{described_node_type_with_input_ports, read, refusal_of};
use super::*;
use crate::core::descriptors::{PinGroupSlotCount, PinType};

/// A class node holding `pin_groups`, as bare `@node` stamps them.
fn a_described_class_node_with_pin_groups(pin_groups: serde_json::Value) -> serde_json::Value {
    let mut described = described_node_type_with_input_ports(serde_json::json!([]));
    described["pin_groups"] = pin_groups;
    described
}

/// The group `layers`, each slot a picture and its opacity, as `PinGroup[Layer]` stamps it.
fn layers() -> serde_json::Value {
    serde_json::json!({
        "name": "layers",
        "description": "The layers, the bottom one first.",
        "inputs": [
            {
                "name": "picture", "description": "", "family": {"family": "image"},
                "pin_declaration": {
                    "pin_type": {"kind": "is", "pin_type": {"type": "texture"}},
                    "flow": "stream",
                },
            },
            {
                "name": "opacity", "description": "", "family": {"family": "channels"},
                "pin_declaration": {
                    "pin_type": {"kind": "is", "pin_type": {"type": "float"}},
                    "flow": "value",
                },
                "pin_default": 1.0,
                "input_mode": {"mode": "wire_or_value"},
            },
        ],
        "outputs": [],
    })
}

#[test]
fn a_class_nodes_pin_group_reads_into_its_descriptor_typed() {
    let declaration = read(&a_described_class_node_with_pin_groups(serde_json::json!(
        [layers()]
    )))
    .expect("the group is read");

    let [group] = declaration.descriptor.pin_groups.as_slice() else {
        panic!("one group is read");
    };
    assert_eq!(group.name, "layers");
    assert_eq!(group.description, "The layers, the bottom one first.");
    assert_eq!(group.slot_count(), PinGroupSlotCount::FollowsItsWires);
    let [picture, opacity] = group.inputs.as_slice() else {
        panic!("the slot holds two inputs");
    };
    assert!(matches!(
        picture.declared_pin().map(|pin| pin.pin_type),
        Some(DeclaredPinType::Is {
            pin_type: PinType::Texture { .. }
        })
    ));
    assert_eq!(opacity.pin_default, Some(serde_json::json!(1.0)));
    assert_eq!(opacity.input_mode, Some(NodeInputMode::WireOrValue));
    let one_slot = declaration.descriptor.with_one_slot_in_each_pin_group();
    assert!(
        one_slot
            .inputs
            .iter()
            .any(|input| input.name == "layers.0.opacity")
    );
}

#[test]
fn a_class_node_declaring_no_pin_group_reads_with_none() {
    let declaration = read(&described_node_type_with_input_ports(serde_json::json!([])))
        .expect("the node is read");

    assert!(declaration.descriptor.pin_groups.is_empty());
}

#[test]
fn a_group_holding_only_outputs_has_its_count_set() {
    let elements = serde_json::json!({
        "name": "elements", "description": "",
        "inputs": [],
        "outputs": [{
            "name": "element", "description": "", "family": {"family": "channels"},
            "pin_declaration": {
                "pin_type": {"kind": "is", "pin_type": {"type": "float"}},
                "flow": "value",
            },
        }],
    });

    let declaration = read(&a_described_class_node_with_pin_groups(serde_json::json!(
        [elements]
    )))
    .expect("the group is read");

    assert_eq!(
        declaration.descriptor.pin_groups[0].slot_count(),
        PinGroupSlotCount::SetByItsInput
    );
}

#[test]
fn a_pin_group_no_node_type_may_declare_is_refused_naming_it() {
    let mut named_with_the_separator = layers();
    named_with_the_separator["name"] = serde_json::json!("lay.ers");
    let mut holding_no_pin = layers();
    holding_no_pin["inputs"] = serde_json::json!([]);

    assert!(
        refusal_of(&a_described_class_node_with_pin_groups(serde_json::json!(
            [named_with_the_separator]
        )))
        .contains("\"lay.ers\"")
    );
    assert!(
        refusal_of(&a_described_class_node_with_pin_groups(serde_json::json!(
            [holding_no_pin]
        )))
        .contains("holds no pin in its slot")
    );
    assert!(
        refusal_of(&a_described_class_node_with_pin_groups(
            serde_json::json!({"name": "layers"})
        ))
        .contains("pin_groups must be a list")
    );
}

#[test]
fn a_slot_input_declaring_no_family_is_refused_naming_the_group() {
    let mut no_family = layers();
    no_family["inputs"][0]
        .as_object_mut()
        .expect("an input is a dict")
        .remove("family");

    assert!(
        refusal_of(&a_described_class_node_with_pin_groups(serde_json::json!(
            [no_family]
        )))
        .contains("port \"picture\" declares no family")
    );
}
