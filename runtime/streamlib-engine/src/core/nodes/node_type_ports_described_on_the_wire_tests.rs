// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use serde_json::json;
use streamlib_runtime_client_contract::local_api_wire_contract::NodeTypeInputMode;

use super::NodeTypePortsDescribedOnTheWire;
use crate::core::descriptors::{
    FunctionNodeLanguage, NodeClassImportPath, NodeClassShortName, NodeDescriptor, NodeInputMode,
    NodeKind, PinDeclaration, PinType, PortDescriptor, SettingReadTime,
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
    .with_input(PortDescriptor::new(
        "label",
        "",
        false,
        PinDeclaration::exactly(PinType::String {}),
    ))
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
    .with_input(PortDescriptor::new(
        "kind",
        "",
        false,
        PinDeclaration::exactly(PinType::a_choice_of_any()),
    ));

    let described = NodeTypePortsDescribedOnTheWire::of(&descriptor);

    assert_eq!(
        described.inputs[0].mode,
        Some(NodeTypeInputMode::SettingOnly)
    );
    assert!(described.inputs[0].value_json_schema.is_some());
    assert!(!described.inputs[0].takes_a_wire);
}

/// Each input says whether it takes a wire as the link check rules: an input read each run and
/// a setting read at setup take one; a setting read each run, and every setting of a
/// `GraphInput`, take none.
///
/// Mental-revert: reading the mark from the mode alone says a `GraphInput`'s `port` takes one.
#[test]
fn each_input_says_whether_it_takes_a_wire_as_the_link_check_rules() {
    let float = || PinDeclaration::exactly(PinType::Float { exact_size: None });
    let an_input = |name: &str, mode: NodeInputMode| {
        PortDescriptor::new(name, "", false, float()).with_input_mode(mode)
    };
    let descriptor = NodeDescriptor::new(
        NodeClassShortName::new("Blur").unwrap(),
        NodeClassImportPath::new("nodes.blur:Blur").unwrap(),
        "",
    )
    .with_input(an_input("amount", NodeInputMode::WireOrValue))
    .with_input(an_input(
        "radius",
        NodeInputMode::SettingOnly {
            read: SettingReadTime::AtSetup,
        },
    ))
    .with_input(an_input(
        "gain",
        NodeInputMode::SettingOnly {
            read: SettingReadTime::EachRun,
        },
    ));

    let takes_a_wire: Vec<(String, bool)> = NodeTypePortsDescribedOnTheWire::of(&descriptor)
        .inputs
        .into_iter()
        .map(|input| (input.port.name, input.takes_a_wire))
        .collect();
    assert_eq!(
        takes_a_wire,
        [
            ("amount".to_owned(), true),
            ("radius".to_owned(), true),
            ("gain".to_owned(), false),
        ]
    );

    let graph_input = NodeTypePortsDescribedOnTheWire::of(
        &crate::core::nodes::graph_interface_node_types::the_node_type_of_a_graph_input()
            .expect("the GraphInput node type"),
    );
    let port = graph_input
        .inputs
        .iter()
        .find(|input| input.port.name == "port")
        .expect("a GraphInput has a `port` setting");
    assert!(!port.takes_a_wire, "a GraphInput's settings take no wire");
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

/// The value schema `describe_node_types` states for the setting `name` of a type whose config
/// schema is `config_schema`, and a validator built from it.
fn the_value_schema_of_the_setting(
    config_schema: serde_json::Value,
    name: &str,
) -> (serde_json::Value, jsonschema::Validator) {
    let descriptor = NodeDescriptor::new(
        NodeClassShortName::new("Compositor").unwrap(),
        NodeClassImportPath::new("nodes.compose:Compositor").unwrap(),
        "",
    )
    .with_config_schema(config_schema);
    let value_json_schema = NodeTypePortsDescribedOnTheWire::of(&descriptor)
        .inputs
        .into_iter()
        .find(|input| input.port.name == name)
        .expect("the config field is described as a setting")
        .value_json_schema
        .expect("every setting states the schema of its value");
    let validator =
        jsonschema::validator_for(&value_json_schema).expect("the value schema stands alone");
    (value_json_schema, validator)
}

/// A setting no pin type holds exactly, such as a list of layer placements or an integer of no
/// stated size, states its config field's own schema as the schema of its value.
///
/// Mental-revert: taking a setting's value schema from its pin type alone states none for
/// `layers` and `width`.
#[test]
fn a_setting_no_pin_type_holds_states_its_fields_own_schema() {
    let config_schema = json!({
        "type": "object",
        "properties": {
            "width": { "type": "integer", "minimum": 1, "default": 1920 },
            "layers": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "x": { "type": "integer", "default": 0 },
                        "opacity": { "type": "number", "minimum": 0.0, "maximum": 1.0 },
                    },
                    "additionalProperties": false,
                },
            },
        },
        "additionalProperties": false,
    });

    let (_, layers) = the_value_schema_of_the_setting(config_schema.clone(), "layers");
    assert!(layers.is_valid(&json!([{ "x": 40, "opacity": 0.5 }])));
    assert!(!layers.is_valid(&json!([{ "opacity": 2.0 }])));
    assert!(!layers.is_valid(&json!({ "x": 40 })));

    let (_, width) = the_value_schema_of_the_setting(config_schema, "width");
    assert!(width.is_valid(&json!(1280)));
    assert!(!width.is_valid(&json!(0)));
}

/// A setting whose field refers to the config schema's definitions states a schema that carries
/// them, so each reference resolves; one that refers to none carries none.
///
/// Mental-revert: stating the field's schema without the definitions leaves `#/$defs/Placement`
/// unresolvable, and no validator builds from it.
#[test]
fn a_settings_value_schema_carries_the_definitions_its_field_refers_to() {
    let config_schema = json!({
        "type": "object",
        "$defs": {
            "Placement": {
                "type": "object",
                "properties": { "opacity": { "type": "number", "minimum": 0.0, "maximum": 1.0 } },
                "required": ["opacity"],
            },
        },
        "properties": {
            "layers": { "type": "array", "items": { "$ref": "#/$defs/Placement" } },
            "first": { "$ref": "#/$defs/Placement" },
            "label": { "type": ["string", "null"] },
        },
    });

    let (_, layers) = the_value_schema_of_the_setting(config_schema.clone(), "layers");
    assert!(layers.is_valid(&json!([{ "opacity": 0.25 }])));
    assert!(!layers.is_valid(&json!([{ "opacity": 3.0 }])));

    let (_, first) = the_value_schema_of_the_setting(config_schema.clone(), "first");
    assert!(first.is_valid(&json!({ "opacity": 1.0 })));
    assert!(!first.is_valid(&json!({})));

    let (label_schema, label) = the_value_schema_of_the_setting(config_schema, "label");
    assert_eq!(label_schema, json!({ "type": ["string", "null"] }));
    assert!(label.is_valid(&serde_json::Value::Null));
}
