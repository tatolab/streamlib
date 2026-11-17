// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use crate::core::descriptors::{NodeClassImportPath, NodeClassShortName, NodeDescriptor};

fn descriptor_carrying(config_schema: Option<serde_json::Value>) -> NodeDescriptor {
    let descriptor = NodeDescriptor::new(
        NodeClassShortName::new("ChannelsToArray").unwrap(),
        NodeClassImportPath::new("streamlib_media_builtins::port_family_conversion").unwrap(),
        "a probe",
    );
    match config_schema {
        Some(document) => descriptor.with_config_schema(document),
        None => descriptor,
    }
}

/// The control plane serves the descriptor's document, not a summary of
/// it: a field's type, its description and its default all survive the
/// hop, because the MCP catalog reads the same rendering.
#[test]
fn a_registered_descriptors_config_schema_reaches_the_rendering_unchanged() {
    let document = serde_json::json!({
        "type": "object",
        "properties": {
            "width": { "type": "integer", "description": "Frame width in pixels.", "default": 1280 },
            "height": { "type": "integer", "description": "Frame height in pixels.", "default": 720 },
        },
    });
    let rendered = serde_json::to_value(NodeDescriptorOutput::from(&descriptor_carrying(Some(
        document.clone(),
    ))))
    .unwrap();
    assert_eq!(rendered["config_schema"], document);
}

#[test]
fn a_descriptor_carrying_no_config_schema_renders_no_key_rather_than_a_null() {
    let rendered =
        serde_json::to_value(NodeDescriptorOutput::from(&descriptor_carrying(None))).unwrap();
    assert!(rendered.get("config_schema").is_none(), "{rendered}");
}

/// A Python class node's settings as its config class derives them: each field the schema
/// lists, after every input port, as an input that takes no wire, read at setup, with its
/// default, typed where its schema is exact; free-form data and a field that may be null carry
/// no pin type.
///
/// Mental-revert: rendering only the declared inputs leaves `inputs` holding `video` alone.
#[test]
fn each_config_field_is_an_input_taking_no_wire_with_its_default_and_an_exact_type_only() {
    let mut descriptor = descriptor_carrying(Some(serde_json::json!({
        "type": "object",
        "properties": {
            "gain": {"type": "number", "default": 0.5, "description": "How loud."},
            "frames": {"type": "integer", "default": 30},
            "mirrored": {"type": "boolean", "default": false},
            "title": {"type": "string", "default": ""},
            "waveform": {"enum": ["sine", "square"], "default": "sine"},
            "asset": {"type": "string", "default": "",
                      "x-tatolab-project-asset": {"media": ["image"]}},
            "layers": {"type": "array", "default": []},
            "extra": {"type": "object"},
            "label": {"anyOf": [{"type": "string"}, {"type": "null"}], "default": null},
        },
    })));
    descriptor.inputs = vec![crate::core::descriptors::PortDescriptor::new(
        "video",
        "the picture",
        false,
        crate::core::descriptors::PortFamilyDeclaration::of_family(
            crate::core::descriptors::PortFamily::Image,
        ),
    )];
    let rendered = serde_json::to_value(NodeDescriptorOutput::from(&descriptor)).unwrap();
    let inputs = rendered["inputs"].as_array().unwrap();
    let names: Vec<&str> = inputs
        .iter()
        .map(|input| input["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        [
            "video", "gain", "frames", "mirrored", "title", "waveform", "asset", "layers", "extra",
            "label"
        ]
    );
    assert!(inputs[0].get("input_mode").is_none(), "{}", inputs[0]);
    assert_eq!(
        inputs[1],
        serde_json::json!({
            "name": "gain", "description": "How loud.", "required": false,
            "delivery_profile": null,
            "pin_type": {"kind": "is", "pin_type": {"type": "float"}}, "flow": "value",
            "input_mode": {"mode": "setting_only", "read": "at_setup"},
            "default_value": 0.5,
        })
    );
    let pin_type_of = |name: &str| {
        inputs
            .iter()
            .find(|input| input["name"] == name)
            .unwrap()
            .get("pin_type")
            .map(|declared| declared["pin_type"].clone())
    };
    assert_eq!(
        pin_type_of("frames"),
        Some(serde_json::json!({"type": "integer"}))
    );
    assert_eq!(
        pin_type_of("mirrored"),
        Some(serde_json::json!({"type": "boolean"}))
    );
    assert_eq!(
        pin_type_of("title"),
        Some(serde_json::json!({"type": "string"}))
    );
    assert_eq!(
        pin_type_of("waveform"),
        Some(serde_json::json!({"type": "menu", "option_names": ["sine", "square"]}))
    );
    assert_eq!(
        pin_type_of("asset"),
        Some(serde_json::json!({"type": "string"}))
    );
    for free_form in ["layers", "extra", "label"] {
        assert_eq!(pin_type_of(free_form), None, "{free_form}");
    }
    let label = inputs
        .iter()
        .find(|input| input["name"] == "label")
        .unwrap();
    assert!(label.get("default_value").is_none(), "{label}");
    assert!(
        inputs
            .iter()
            .skip(1)
            .all(|input| input.get("family").is_none()),
        "{rendered}"
    );
    assert_eq!(
        rendered["exposable_config_fields"],
        serde_json::json!(["gain", "frames", "mirrored"])
    );
    assert_eq!(
        rendered["config_schema"]["properties"]["gain"]["default"],
        0.5
    );
}

/// A built-in's settings as `#[node_config]` derives them: an integer of 32 bits keeps its size,
/// one of 64 bits has no pin type, and a field reached through a definition is read through it.
#[test]
fn a_built_ins_settings_are_typed_by_their_rust_format_and_a_definition_is_read_through() {
    let descriptor = descriptor_carrying(Some(serde_json::json!({
        "type": "object",
        "properties": {
            "sample_rate": {"type": "integer", "format": "uint32", "minimum": 0.0,
                            "default": 48000},
            "frame_count": {"type": "integer", "format": "uint64", "minimum": 0.0},
            "mode": {"allOf": [{"$ref": "#/$defs/Mode"}], "description": "Which way.",
                     "default": "fast"},
        },
        "$defs": {"Mode": {"type": "string", "oneOf": [
            {"type": "string", "enum": ["fast"], "description": "quick"},
            {"type": "string", "const": "slow"},
        ]}},
    })));
    let rendered = serde_json::to_value(NodeDescriptorOutput::from(&descriptor)).unwrap();
    let input = |name: &str| {
        rendered["inputs"]
            .as_array()
            .unwrap()
            .iter()
            .find(|input| input["name"] == name)
            .unwrap()
            .clone()
    };
    assert_eq!(
        input("sample_rate")["pin_type"]["pin_type"],
        serde_json::json!({"type": "integer", "exact_size": "uint32"})
    );
    assert_eq!(input("sample_rate")["default_value"], 48000);
    assert!(input("frame_count").get("pin_type").is_none());
    assert_eq!(
        (
            &input("mode")["pin_type"]["pin_type"],
            &input("mode")["description"]
        ),
        (
            &serde_json::json!({"type": "menu", "option_names": ["fast", "slow"]}),
            &serde_json::json!("Which way.")
        )
    );
}
