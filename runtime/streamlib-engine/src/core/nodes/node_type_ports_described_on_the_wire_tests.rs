// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use serde_json::json;
use streamlib_runtime_client_contract::local_api_wire_contract::NodeTypeInputMode;

use super::NodeTypePortsDescribedOnTheWire;
use crate::core::descriptors::{
    FunctionNodeLanguage, NodeClassImportPath, NodeClassShortName, NodeDescriptor, NodeKind,
    PinDeclaration, PinType, PortDescriptor, PortFamily, PortFamilyDeclaration,
};

/// A node type of `node_kind` with one String input, `label`, that carries Records.
fn a_node_type_reading_a_string_as_records(node_kind: NodeKind) -> NodeDescriptor {
    NodeDescriptor {
        node_kind,
        ..NodeDescriptor::new(
            NodeClassShortName::new("Labeller").unwrap(),
            NodeClassImportPath::new("nodes.labels:Labeller").unwrap(),
            "",
        )
    }
    .with_input(
        PortDescriptor::new(
            "label",
            "",
            false,
            PortFamilyDeclaration::of_family(PortFamily::Records),
        )
        .with_pin_declaration(PinDeclaration::exactly(PinType::String {})),
    )
}

/// A String input carrying Records takes no value on a process node, which reads a Records
/// input from its link alone, so it is described as a link alone with no value schema; on a
/// function node, which reads it whole, it takes a link or a value.
///
/// Mental-revert: describing the mode by the pin type alone describes the process node's input
/// as taking a value the engine refuses.
#[test]
fn an_input_takes_a_value_in_its_description_only_where_the_engine_sets_it_one() {
    let on_a_process_node = NodeTypePortsDescribedOnTheWire::of(
        &a_node_type_reading_a_string_as_records(NodeKind::Process),
    );
    let on_a_function_node = NodeTypePortsDescribedOnTheWire::of(
        &a_node_type_reading_a_string_as_records(NodeKind::Function {
            language: FunctionNodeLanguage::Python,
        }),
    );

    assert_eq!(
        on_a_process_node.inputs[0].mode,
        Some(NodeTypeInputMode::WireOnly)
    );
    assert_eq!(on_a_process_node.inputs[0].value_json_schema, None);
    assert_eq!(
        on_a_function_node.inputs[0].mode,
        Some(NodeTypeInputMode::WireOrValue)
    );
    assert_eq!(
        on_a_function_node.inputs[0].value_json_schema,
        Some(json!({ "type": "string" }))
    );
}

/// An input whose pin type never takes a wire, declaring no mode, is described as a setting.
///
/// Mental-revert: describing it as a link or a value advertises a wire the link check refuses.
#[test]
fn an_input_whose_pin_type_takes_no_wire_is_described_as_a_setting() {
    let descriptor = NodeDescriptor::new(
        NodeClassShortName::new("Chooser").unwrap(),
        NodeClassImportPath::new("nodes.choose:Chooser").unwrap(),
        "",
    )
    .with_input(
        PortDescriptor::new(
            "kind",
            "",
            false,
            PortFamilyDeclaration::of_family(PortFamily::Records),
        )
        .with_pin_declaration(PinDeclaration::exactly(PinType::PinTypeChoice {
            among: Vec::new(),
        })),
    );

    let described = NodeTypePortsDescribedOnTheWire::of(&descriptor);

    assert_eq!(
        described.inputs[0].mode,
        Some(NodeTypeInputMode::SettingOnly)
    );
    assert!(described.inputs[0].value_json_schema.is_some());
}

/// A config field of a type not yet typed carries its schema's range into the value schema its
/// setting states.
///
/// Mental-revert: reading only the field's type publishes a number schema that takes 2.
#[test]
fn a_config_fields_range_reaches_the_value_schema_of_its_setting() {
    let descriptor = NodeDescriptor::new(
        NodeClassShortName::new("Dimmer").unwrap(),
        NodeClassImportPath::new("nodes.dim:Dimmer").unwrap(),
        "",
    )
    .with_config_schema(json!({
        "type": "object",
        "properties": { "gain": { "type": "number", "minimum": 0, "maximum": 1 } },
    }));

    let described = NodeTypePortsDescribedOnTheWire::of(&descriptor);

    let gain = described
        .inputs
        .iter()
        .find(|input| input.port.name == "gain")
        .expect("the config field is described as a setting");
    assert_eq!(
        gain.value_json_schema,
        Some(json!({ "type": "number", "minimum": 0, "maximum": 1 }))
    );
}
