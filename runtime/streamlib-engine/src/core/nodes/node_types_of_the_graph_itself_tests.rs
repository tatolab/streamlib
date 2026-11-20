// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use crate::core::NodeClassImportPath;
use crate::core::descriptors::{
    DeclaredPinType, NodeInputMode, PinType, PortDescriptor, SettingReadTime,
};
use crate::core::graph::PortInfo;
use crate::core::graph::graph_interface_resolution::{
    GRAPH_INPUT_NODE_TYPE, GRAPH_OUTPUT_NODE_TYPE,
};
use crate::core::graph::graph_variable_resolution::{
    GET_VARIABLE_NODE_TYPE, SET_VARIABLE_NODE_TYPE,
};

use super::{GRAPH_NODE_TYPE_CATEGORY, the_node_types_of_the_graph_itself};

#[test]
fn the_four_node_types_of_the_graph_itself_are_listed_under_the_graph_category() {
    let listed = the_node_types_of_the_graph_itself();
    let names: Vec<&str> = listed
        .iter()
        .map(|descriptor| descriptor.node_class_import_path.as_str())
        .collect();
    assert_eq!(
        names,
        [
            GRAPH_INPUT_NODE_TYPE,
            GRAPH_OUTPUT_NODE_TYPE,
            SET_VARIABLE_NODE_TYPE,
            GET_VARIABLE_NODE_TYPE
        ]
    );
    for descriptor in &listed {
        assert_eq!(
            descriptor.category.as_deref(),
            Some(GRAPH_NODE_TYPE_CATEGORY),
            "{}",
            descriptor.node_class_import_path
        );
        for port in descriptor.inputs.iter().chain(&descriptor.outputs) {
            assert!(
                port.pin_declaration.is_some(),
                "port `{}` of {} is typed",
                port.name,
                descriptor.node_class_import_path
            );
        }
    }
}

#[test]
fn a_set_variable_takes_any_type_on_value_and_a_get_variable_gives_any_type_on_value() {
    let listed = the_node_types_of_the_graph_itself();
    let of = |node_type: &str| {
        listed
            .iter()
            .find(|descriptor| descriptor.node_class_import_path.as_str() == node_type)
            .expect("the node type is listed")
    };
    let set_value = of(SET_VARIABLE_NODE_TYPE)
        .inputs
        .iter()
        .find(|input| input.name == "value")
        .expect("a SetVariable takes `value`");
    assert!(takes_a_wire(SET_VARIABLE_NODE_TYPE, set_value));
    assert_eq!(
        set_value.pin_declaration.as_ref().map(|pin| &pin.pin_type),
        Some(&DeclaredPinType::Any)
    );
    let get = of(GET_VARIABLE_NODE_TYPE);
    assert_eq!(
        get.inputs
            .iter()
            .map(|input| (input.name.as_str(), input.input_mode))
            .collect::<Vec<_>>(),
        [(
            "variable",
            Some(NodeInputMode::SettingOnly {
                read: SettingReadTime::AtSetup
            })
        )],
        "a GetVariable takes only its `variable`, a setting read at setup"
    );
    assert_eq!(
        get.outputs
            .iter()
            .map(|output| (
                output.name.as_str(),
                output.pin_declaration.as_ref().map(|pin| &pin.pin_type)
            ))
            .collect::<Vec<_>>(),
        [("value", Some(&DeclaredPinType::Any))]
    );
    assert_eq!(
        get.inputs[0]
            .pin_declaration
            .as_ref()
            .map(|pin| &pin.pin_type),
        Some(&DeclaredPinType::Is {
            pin_type: PinType::String {}
        })
    );
}

/// Whether `input` of a node of `node_type` takes a wire, by the runtime's one rule.
fn takes_a_wire(node_type: &str, input: &PortDescriptor) -> bool {
    PortInfo::from(input).takes_a_wire_on_a_node_of(
        &NodeClassImportPath::new(node_type).expect("a legal import path"),
    )
}
