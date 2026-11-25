// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use crate::core::descriptors::{NodeClassImportPath, NodeClassShortName, NodeDescriptor};

fn descriptor_carrying(config_schema: Option<serde_json::Value>) -> NodeDescriptor {
    let descriptor = NodeDescriptor::new(
        NodeClassShortName::new("AudioToTensor").unwrap(),
        NodeClassImportPath::new("streamlib_media_builtins::pin_type_conversion").unwrap(),
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

/// The input named `name` of `rendered`, a rendered catalog entry.
fn the_rendered_input<'rendered>(
    rendered: &'rendered serde_json::Value,
    name: &str,
) -> &'rendered serde_json::Value {
    rendered["inputs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|input| input["name"] == name)
        .unwrap_or_else(|| panic!("no input `{name}` in {rendered}"))
}

/// The pin type the input named `name` of `rendered` declares; `None` when it declares none.
fn the_pin_type_of_the_rendered_input(
    rendered: &serde_json::Value,
    name: &str,
) -> Option<serde_json::Value> {
    the_rendered_input(rendered, name)
        .get("pin_type")
        .map(|declared| declared["pin_type"].clone())
}

/// A Python class node's settings as its config class derives them: each field the schema
/// lists, after every input port, as an input that takes no wire, read at setup, with its
/// default, typed where its schema is exact. Free-form data, a field that may be null and a
/// Python `int`, which states no size, carry no pin type; an asset held as a bare string is a
/// String carrying its project asset annotation.
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
    let names: Vec<&str> = rendered["inputs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|input| input["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        [
            "video", "gain", "frames", "mirrored", "title", "waveform", "layers", "extra", "label"
        ]
    );
    let video = the_rendered_input(&rendered, "video");
    assert!(video.get("input_mode").is_none(), "{video}");
    assert!(video.get("default_value").is_none(), "{video}");
    assert_eq!(
        the_rendered_input(&rendered, "gain"),
        &serde_json::json!({
            "name": "gain", "description": "How loud.", "required": false,
            "delivery_profile": null,
            "pin_type": {"kind": "is", "pin_type": {"type": "float"}}, "flow": "value",
            "input_mode": {"mode": "setting_only", "read": "at_setup"},
            "default_value": 0.5,
        })
    );
    assert_eq!(
        the_pin_type_of_the_rendered_input(&rendered, "mirrored"),
        Some(serde_json::json!({"type": "boolean"}))
    );
    assert_eq!(
        the_pin_type_of_the_rendered_input(&rendered, "title"),
        Some(serde_json::json!({"type": "string"}))
    );
    assert_eq!(
        the_pin_type_of_the_rendered_input(&rendered, "waveform"),
        Some(serde_json::json!({"type": "menu", "option_names": ["sine", "square"]}))
    );
    for with_no_exact_type in ["frames", "layers", "extra", "label"] {
        assert_eq!(
            the_pin_type_of_the_rendered_input(&rendered, with_no_exact_type),
            None,
            "{with_no_exact_type}"
        );
    }
    assert_eq!(the_rendered_input(&rendered, "frames")["default_value"], 30);
    let label = the_rendered_input(&rendered, "label");
    assert!(label.get("default_value").is_none(), "{label}");
    assert!(
        rendered["inputs"]
            .as_array()
            .unwrap()
            .iter()
            .skip(1)
            .all(|input| input.get("family").is_none() && input.get("annotations").is_none()),
        "{rendered}"
    );
    assert!(
        rendered.get("exposable_config_fields").is_none(),
        "{rendered}"
    );
    assert_eq!(
        rendered["config_schema"]["properties"]["gain"]["default"],
        0.5
    );
}

/// `ImageFile`'s `asset`, its project image's UUID held as a bare string, as its config class
/// derives it: a String setting carrying the project asset annotation its schema states, so the
/// asset picker still serves it, with `config_schema` unchanged.
///
/// Mental-revert: leaving the annotation off renders `asset` with no `annotations`.
#[test]
fn image_files_asset_is_a_string_setting_carrying_its_project_asset_annotation() {
    let config_schema = serde_json::json!({
        "type": "object",
        "properties": {
            "asset": {"type": "string", "default": "",
                      "x-tatolab-project-asset": {"media": ["image"]}},
        },
    });
    let descriptor = NodeDescriptor::new(
        NodeClassShortName::new("ImageFile").unwrap(),
        NodeClassImportPath::new("core_nodes.image_file:ImageFile").unwrap(),
        "Publishes a project image",
    )
    .with_config_schema(config_schema.clone());
    let rendered = serde_json::to_value(NodeDescriptorOutput::from(&descriptor)).unwrap();
    let asset = the_rendered_input(&rendered, "asset");
    assert_eq!(
        (
            &asset["pin_type"]["pin_type"],
            &asset["annotations"],
            &asset["default_value"]
        ),
        (
            &serde_json::json!({"type": "string"}),
            &serde_json::json!({"project_asset": {"media": ["image"]}}),
            &serde_json::json!("")
        ),
        "{asset}"
    );
    assert_eq!(rendered["config_schema"], config_schema);
}

/// A built-in's config as its schema document derives it: an integer of 32 bits keeps its size,
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
    assert_eq!(
        the_pin_type_of_the_rendered_input(&rendered, "sample_rate"),
        Some(serde_json::json!({"type": "integer", "exact_size": "uint32"}))
    );
    assert_eq!(
        the_rendered_input(&rendered, "sample_rate")["default_value"],
        48000
    );
    assert_eq!(
        the_pin_type_of_the_rendered_input(&rendered, "frame_count"),
        None
    );
    assert_eq!(
        (
            the_pin_type_of_the_rendered_input(&rendered, "mode"),
            &the_rendered_input(&rendered, "mode")["description"]
        ),
        (
            Some(serde_json::json!({"type": "menu", "option_names": ["fast", "slow"]})),
            &serde_json::json!("Which way.")
        )
    );
}

/// The catalog's schema names every kind a node type runs as and every language a function node
/// is written in, as a rendering writes them.
#[test]
fn the_catalog_schema_names_each_kind_and_language_a_node_type_renders() {
    let schema = serde_json::to_value(schemars::schema_for!(NodeDescriptorOutput)).unwrap();
    let properties = &schema["properties"];
    assert_eq!(
        (&properties["kind"]["enum"], &properties["language"]["enum"]),
        (
            &serde_json::json!(["shader", "function", "process", "fragment"]),
            &serde_json::json!(["python", "rust"])
        ),
        "{properties}"
    );
}
