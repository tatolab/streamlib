// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use serde_json::json;

fn descriptor_from(descriptor: Value) -> BuiltInNodeDescriptor {
    serde_json::from_value(descriptor).unwrap()
}

/// A built-in taking no config whose every port states one pin type, as the runtime describes
/// it: a Stream input, two settings read at setup, a Menu among them, and an output.
fn texture_tinter_descriptor_with_settings() -> Value {
    json!({
        "type": "tatolab.stream:TextureTinter",
        "description": "Tints textures",
        "config_schema": null,
        "inputs": [
            {
                "name": "image", "description": "The textures to tint",
                "required": true, "delivery_profile": "newest",
                "pin_type": {"kind": "is", "pin_type": {"type": "texture"}}, "flow": "stream"
            },
            {
                "name": "strength", "description": "How strongly each is tinted.",
                "required": false, "delivery_profile": null,
                "pin_type": {"kind": "is", "pin_type": {"type": "integer", "exact_size": "uint8"}},
                "flow": "value",
                "input_mode": {"mode": "setting_only", "read": "at_setup"},
                "default_value": 3
            },
            {
                "name": "tint", "description": "The tint.",
                "required": false, "delivery_profile": null,
                "pin_type": {"kind": "is", "pin_type": {"type": "menu", "option_names": ["warm", "cool"]}},
                "flow": "value",
                "input_mode": {"mode": "setting_only", "read": "at_setup"},
                "default_value": "warm"
            }
        ],
        "outputs": [{
            "name": "tinted", "description": "One tinted texture each",
            "required": true, "delivery_profile": null,
            "pin_type": {"kind": "is", "pin_type": {"type": "texture"}}, "flow": "stream"
        }]
    })
}

/// A built-in renders as a class node is declared: its Stream input and output pins, and its
/// settings fields with their types and defaults.
#[test]
fn a_built_in_renders_its_fields_and_pins() {
    let rendered = render_built_in_node_classes_module(&[descriptor_from(
        texture_tinter_descriptor_with_settings(),
    )])
    .unwrap();
    let golden = format!(
        "{GENERATED_MODULE_HEADER}{}",
        r#"
from typing import Annotated, ClassVar, Literal

from ._built_in_node import BuiltInNode
from ._class_node_pins import InputPin, OutputPin
from ._description import Description
from ._input_declaration_annotations import SettingOnly, StreamDelivery
from ._pin_types import Texture, UInt8

__all__ = [
    "TextureTinter",
]


class TextureTinter(BuiltInNode):
    """Tints textures.

    Inputs:
        `image` (newest): The textures to tint.

    Outputs:
        `tinted`: One tinted texture each.
    """

    type: ClassVar[str] = "tatolab.stream:TextureTinter"

    image: InputPin[
        Annotated[
            Texture,
            StreamDelivery("newest"),
            Description("The textures to tint"),
        ]
    ]

    strength: Annotated[
        UInt8,
        SettingOnly(),
        Description("How strongly each is tinted."),
    ] = 3

    tint: Annotated[
        Literal["warm", "cool"],
        SettingOnly(),
        Description("The tint."),
    ] = "warm"

    tinted: OutputPin[
        Annotated[
            Texture,
            Description("One tinted texture each"),
        ]
    ]
"#
    );
    assert_eq!(rendered, golden, "rendered:\n{rendered}");
}

#[test]
fn a_typed_built_ins_setting_with_no_default_is_refused_naming_it() {
    let mut descriptor = texture_tinter_descriptor_with_settings();
    descriptor["inputs"][1]
        .as_object_mut()
        .unwrap()
        .remove("default_value");
    let refusal = render_built_in_node_classes_module(&[descriptor_from(descriptor)])
        .unwrap_err()
        .to_string();
    assert!(
        refusal.contains("input `strength` holds no default"),
        "{refusal}"
    );
}

#[test]
fn an_image_outputs_frame_rate_renders_under_the_port() {
    let descriptor = json!({
        "type": "tatolab.stream:FrameSource",
        "description": "Frames",
        "inputs": [],
        "outputs": [{
            "name": "video",
            "description": "Frames",
            "required": true,
            "delivery_profile": null,
            "pin_type": {"kind": "is", "pin_type": {"type": "texture"}},
            "flow": "stream",
            "family": {
                "family": "image",
                "frame_rate": {"stated": {"numerator": 30000, "denominator": 1001}}
            }
        }, {
            "name": "preview",
            "description": "Preview",
            "required": true,
            "delivery_profile": null,
            "pin_type": {"kind": "is", "pin_type": {"type": "texture"}},
            "flow": "stream",
            "family": {
                "family": "image",
                "frame_rate": {"from_config": {"config_key": "frames_per_second"}}
            }
        }]
    });
    let rendered = render_built_in_node_classes_module(&[descriptor_from(descriptor)]).unwrap();
    assert!(
        rendered.contains(
            "        `video`: Frames.\n            Frame rate: \
                 `rate=ImageStreamFrameRate(30000, 1001)`.\n"
        ),
        "{rendered}"
    );
    assert!(
        rendered.contains("Frame rate: its `frames_per_second` setting."),
        "{rendered}"
    );
}

#[test]
fn a_long_description_wraps_before_the_column_and_list_items_hang() {
    let docstring = render_docstring(
        &[
            "word ".repeat(40),
            format!("Values:\n    `fit`: {}", "item ".repeat(30)),
        ],
        4,
    );
    for line in docstring.lines() {
        assert!(line.chars().count() <= DOCSTRING_WRAP_COLUMN, "{line:?}");
    }
    assert!(docstring.contains("\n        `fit`: item"), "{docstring}");
    assert!(docstring.contains("\n            item"), "{docstring}");
}

/// A built-in whose ports carry the type its `bundle_type` setting chooses renders each as a pin
/// of `object` typed by that setting, and the setting as a pin type choice holding none until it
/// is set.
///
/// Mental-revert: reading only ports that state one type renders the built-in untyped, with an
/// empty config class and no pins.
#[test]
fn a_port_typed_by_a_setting_renders_as_a_pin_typed_by_that_setting() {
    let typed_by_the_setting = json!({"kind": "set_by_input", "input_name": "bundle_type"});
    let rendered = render_built_in_node_classes_module(&[descriptor_from(json!({
        "type": "tatolab.stream:Carrier",
        "description": "Carries a bundle",
        "inputs": [
            {"name": "carried", "description": "What is carried", "delivery_profile": "ordered",
             "pin_type": typed_by_the_setting, "flow": "value"},
            {"name": "bundle_type", "description": "The bundle carried",
             "pin_type": {"kind": "is", "pin_type": {"type": "pin_type_choice"}}, "flow": "value",
             "input_mode": {"mode": "setting_only", "read": "at_setup"}}
        ],
        "outputs": [
            {"name": "passed", "description": "What passes on", "pin_type": typed_by_the_setting,
             "flow": "value"}
        ]
    }))])
    .unwrap();

    assert!(
        rendered.contains("class Carrier(BuiltInNode):"),
        "{rendered}"
    );
    assert!(
        rendered.contains(
            "    carried: InputPin[\n        Annotated[\n            object,\n            \
             PinTypeFromSetting(\"bundle_type\"),\n"
        ),
        "{rendered}"
    );
    assert!(
        rendered.contains(
            "    bundle_type: Annotated[\n        PinTypeChoice | None,\n        SettingOnly(),\n"
        ),
        "{rendered}"
    );
    assert!(rendered.contains("    ] = None\n"), "{rendered}");
    assert!(
        rendered.contains(
            "    passed: OutputPin[\n        Annotated[\n            object,\n            \
             PinTypeFromSetting(\"bundle_type\"),\n"
        ),
        "{rendered}"
    );
    assert!(
        rendered.contains("from ._pin_type_choice import PinTypeChoice, PinTypeFromSetting\n"),
        "{rendered}"
    );
}

/// A built-in taking a config is refused by name, since nothing renders a config any more.
///
/// Mental-revert: rendering it untyped puts a class with no fields or pins in the module, and
/// `stream_builder.add` checks none of the values it is given.
#[test]
fn a_built_in_that_takes_a_config_is_refused_by_name() {
    let mut descriptor = texture_tinter_descriptor_with_settings();
    descriptor["config_schema"] = json!({
        "type": "object",
        "properties": {"path": {"description": "The file.", "type": "string", "default": "out"}}
    });
    let refusal = render_built_in_node_classes_module(&[descriptor_from(descriptor)])
        .unwrap_err()
        .to_string();
    assert!(refusal.contains("built-in `TextureTinter` takes a config"), "{refusal}");
}

/// A built-in with a port that states no pin type is refused by name.
#[test]
fn a_built_in_with_a_port_of_no_pin_type_is_refused_by_name() {
    let mut descriptor = texture_tinter_descriptor_with_settings();
    descriptor["outputs"][0].as_object_mut().unwrap().remove("pin_type");
    let refusal = render_built_in_node_classes_module(&[descriptor_from(descriptor)])
        .unwrap_err()
        .to_string();
    assert!(refusal.contains("a port that states no one pin type"), "{refusal}");
}

#[test]
fn a_test_named_class_opts_out_of_pytest() {
    let mut descriptor = texture_tinter_descriptor_with_settings();
    descriptor["type"] = json!("tatolab.stream:TestTinter");
    let rendered = render_built_in_node_classes_module(&[descriptor_from(descriptor)]).unwrap();
    assert!(rendered.contains("class TestTinter(BuiltInNode):"), "{rendered}");
    assert!(
        rendered.contains("    __test__: ClassVar[bool] = False\n"),
        "{rendered}"
    );
}

/// The catalog lists each setting under `inputs`; a field renders it, so the docstring's inputs
/// name only the pins.
#[test]
fn a_setting_the_catalog_lists_among_the_inputs_is_no_input_of_the_docstring() {
    let rendered = render_built_in_node_classes_module(&[descriptor_from(
        texture_tinter_descriptor_with_settings(),
    )])
    .unwrap();
    assert!(
        rendered.contains("    Inputs:\n        `image` (newest): The textures to tint.\n\n"),
        "{rendered}"
    );
    assert!(!rendered.contains("`strength`: How strongly"), "{rendered}");
}

#[test]
fn a_description_linking_a_rust_path_is_refused_by_name() {
    let mut descriptor = texture_tinter_descriptor_with_settings();
    descriptor["description"] = json!("Absent means [`TextureTint::Warm`].");
    let refusal = render_built_in_node_classes_module(&[descriptor_from(descriptor)])
        .unwrap_err()
        .to_string();
    assert!(refusal.contains("`TextureTint::Warm`"), "{refusal}");
    assert!(refusal.contains("built-in `TextureTinter`"), "{refusal}");
}

#[test]
fn a_type_outside_the_stream_package_is_refused() {
    let mut descriptor = texture_tinter_descriptor_with_settings();
    descriptor["type"] = json!("streamlib_media_builtins::TextureTinter");
    let refusal = render_built_in_node_classes_module(&[descriptor_from(descriptor)])
        .unwrap_err()
        .to_string();
    assert!(refusal.contains("tatolab.stream:"), "{refusal}");
}
