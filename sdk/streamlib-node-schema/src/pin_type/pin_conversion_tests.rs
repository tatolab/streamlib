// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use crate::descriptors::{NodeDescriptor, PortDescriptor};
use crate::{NodeClassImportPath, NodeClassShortName};

fn a_typed_port(name: &str, pin_type: PinType) -> PortDescriptor {
    PortDescriptor::new(name, name, true, PinDeclaration::exactly(pin_type))
}

fn a_setting(name: &str) -> PortDescriptor {
    a_typed_port(name, PinType::Float { exact_size: None })
        .with_input_mode(NodeInputMode::HandedToItsNode)
}

fn a_node_type(inputs: Vec<PortDescriptor>, outputs: Vec<PortDescriptor>) -> NodeDescriptor {
    let mut descriptor = NodeDescriptor::new(
        NodeClassShortName::new("TensorToTexture").unwrap(),
        NodeClassImportPath::of_built_in_node("TensorToTexture"),
        "converts",
    );
    descriptor.inputs = inputs;
    descriptor.outputs = outputs;
    descriptor
}

fn tensor() -> PinType {
    PinType::Tensor {}
}

fn texture() -> PinType {
    PinType::Texture {
        pixel_formats: vec![TexturePixelFormat::Rgba8Unorm],
    }
}

#[test]
fn a_node_type_that_states_no_conversion_is_none() {
    let descriptor = a_node_type(
        vec![a_typed_port("tensor", tensor())],
        vec![a_typed_port("texture", texture())],
    );
    assert_eq!(descriptor.pin_conversion(), Ok(None));
}

#[test]
fn a_conversion_is_its_one_wired_input_and_its_one_output_beside_any_settings() {
    let descriptor = a_node_type(
        vec![
            a_setting("format"),
            a_typed_port("tensor", tensor()),
            a_setting("scale"),
        ],
        vec![a_typed_port("texture", texture())],
    )
    .stating_a_conversion();
    assert_eq!(
        descriptor.pin_conversion(),
        Ok(Some(PinConversion {
            node_type: NodeClassImportPath::of_built_in_node("TensorToTexture"),
            input_port_name: "tensor".to_owned(),
            input_pin_type: tensor(),
            output_port_name: "texture".to_owned(),
            output_pin_type: texture(),
            output_picked_for_the_input: None,
            saved_value_cast: None,
        }))
    );
}

fn a_value_input(name: &str) -> PortDescriptor {
    PortDescriptor {
        pin_default: Some(serde_json::json!(4_000_000)),
        input_mode: Some(NodeInputMode::WireOrValue),
        ..a_typed_port(name, PinType::Float { exact_size: None })
    }
}

#[test]
fn a_conversion_written_as_one_function_converts_its_one_input_beside_its_value_inputs() {
    let descriptor = a_node_type(
        vec![
            a_value_input("bitrate_bps"),
            a_typed_port("tensor", tensor()),
            a_value_input("fps"),
        ],
        vec![a_typed_port("texture", texture())],
    )
    .stating_a_conversion();
    let conversion = descriptor
        .pin_conversion()
        .expect("a conversion beside value inputs")
        .expect("stated");
    assert_eq!(
        (
            conversion.input_port_name.as_str(),
            conversion.input_pin_type
        ),
        ("tensor", tensor())
    );
}

#[test]
fn a_conversion_whose_one_input_is_a_value_input_converts_that_input() {
    let descriptor = a_node_type(
        vec![a_value_input("level")],
        vec![a_typed_port("texture", texture())],
    )
    .stating_a_conversion();
    let conversion = descriptor
        .pin_conversion()
        .expect("a conversion of one value input")
        .expect("stated");
    assert_eq!(conversion.input_port_name, "level");
}

#[test]
fn a_conversion_of_any_other_shape_is_refused_naming_the_node_type() {
    let two_wired_inputs = a_node_type(
        vec![a_typed_port("a", tensor()), a_typed_port("b", tensor())],
        vec![a_typed_port("texture", texture())],
    );
    let two_outputs = a_node_type(
        vec![a_typed_port("tensor", tensor())],
        vec![a_typed_port("a", texture()), a_typed_port("b", texture())],
    );
    let only_settings = a_node_type(
        vec![a_setting("format")],
        vec![a_typed_port("texture", texture())],
    );
    let an_untyped_input = a_node_type(
        vec![PortDescriptor {
            pin_declaration: None,
            ..a_typed_port("tensor", tensor())
        }],
        vec![a_typed_port("texture", texture())],
    );
    for (descriptor, why) in [
        (
            two_wired_inputs,
            "2 inputs besides its settings and 1 outputs",
        ),
        (two_outputs, "1 inputs besides its settings and 2 outputs"),
        (only_settings, "0 inputs besides its settings and 1 outputs"),
        (
            an_untyped_input,
            "its input `tensor` states no one pin type",
        ),
    ] {
        let refusal = descriptor
            .stating_a_conversion()
            .pin_conversion()
            .expect_err("a conversion of another shape")
            .to_string();
        assert!(
            refusal.contains("`tatolab.stream:TensorToTexture` states that it is a conversion"),
            "{refusal}"
        );
        assert!(refusal.contains(why), "{refusal}");
    }
}

#[test]
fn only_a_built_in_conversion_may_be_inserted() {
    let conversion = |node_type| PinConversion {
        node_type,
        input_port_name: "in".to_owned(),
        input_pin_type: tensor(),
        output_port_name: "out".to_owned(),
        output_pin_type: texture(),
        output_picked_for_the_input: None,
        saved_value_cast: None,
    };
    assert!(conversion(NodeClassImportPath::of_built_in_node("TensorToTexture")).may_be_inserted());
    assert!(
        !conversion(NodeClassImportPath::new("tatolab_codecs:Encoder").unwrap()).may_be_inserted()
    );
}

#[test]
fn a_conversion_whose_wired_pin_takes_no_wire_is_refused() {
    let node_reference = a_typed_port(
        "node",
        PinType::NodeReference {
            referenced_node_type: None,
            one_referrer_per_referenced_node: false,
        },
    );
    let refusal = a_node_type(vec![a_typed_port("tensor", tensor())], vec![node_reference])
        .stating_a_conversion()
        .pin_conversion()
        .expect_err("a node reference takes no wire")
        .to_string();
    assert!(
        refusal.contains("its output `node` is a Node reference, which takes no wire"),
        "{refusal}"
    );
}

#[test]
fn a_value_input_defaulting_to_no_value_is_still_not_what_a_conversion_converts() {
    // `H264Encoder`'s `bitrate_bps: Input[int | None] = Input(None)`, as the engine reads it.
    let bitrate_bps: PortDescriptor = serde_json::from_value(serde_json::json!({
        "name": "bitrate_bps",
        "description": "",
        "required": false,
        "pin_default": null,
        "input_mode": {"mode": "wire_or_value"},
    }))
    .unwrap();
    assert_eq!(bitrate_bps.pin_default, Some(serde_json::Value::Null));
    let descriptor = a_node_type(
        vec![bitrate_bps, a_typed_port("tensor", tensor())],
        vec![a_typed_port("texture", texture())],
    )
    .stating_a_conversion();

    let conversion = descriptor
        .pin_conversion()
        .expect("a conversion beside a value input")
        .expect("stated");

    assert_eq!(conversion.input_port_name, "tensor");
}
