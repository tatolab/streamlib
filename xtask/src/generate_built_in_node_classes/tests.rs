// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use serde_json::json;

/// A sink as the runtime describes a built-in: an input port with a
/// delivery profile, defaults of three JSON types, and an enumeration
/// reached through `$defs` — every shape a built-in config takes today.
fn frame_sink_descriptor_with_every_config_shape() -> Value {
    json!({
        "type": "tatolab.stream:FrameSink",
        "description": "Shows video frames",
        "repository": "https://github.com/tatolab/streamlib",
        "runtime": "rust",
        "config_schema": {
            "title": "FrameSinkConfig",
            "description": "Configuration for [`FrameSink`].",
            "type": "object",
            "properties": {
                "height": {
                    "description": "Initial frame height, in pixels.",
                    "default": 720, "type": "integer", "format": "uint32", "minimum": 0.0
                },
                "scaling": {
                    "description": "How the frame maps onto the sink.",
                    "default": "fit",
                    "allOf": [{"$ref": "#/$defs/FrameSinkScaling"}]
                },
                "title": {"description": "Sink title.", "default": "StreamLib", "type": "string"},
                "device_id": {
                    "description": "Paired with [`Self::height`].",
                    "default": null, "type": ["string", "null"]
                }
            },
            "additionalProperties": false,
            "$defs": {
                "FrameSinkScaling": {
                    "oneOf": [
                        {"description": "Whole frame visible.", "type": "string", "enum": ["fit"]},
                        {"description": "Sink covered.", "type": "string", "enum": ["fill"]}
                    ]
                }
            }
        },
        "inputs": [{
            "name": "video", "description": "Video frames to show",
            "required": true, "delivery_profile": "newest"
        }],
        "outputs": [],
        "examples": {"rust": "", "python": "", "typescript": ""}
    })
}

fn descriptor_from(descriptor: Value) -> BuiltInNodeDescriptor {
    serde_json::from_value(descriptor).unwrap()
}

#[test]
fn one_built_in_renders_to_its_golden_class_and_config() {
    let rendered = render_built_in_node_classes_module(&[descriptor_from(
        frame_sink_descriptor_with_every_config_shape(),
    )])
    .unwrap();
    let golden = format!(
        "{GENERATED_MODULE_HEADER}{}",
        r#"
from typing import ClassVar, Literal, TypedDict

from ._built_in_node import BuiltInNode

__all__ = [
    "FrameSink",
    "FrameSinkConfig",
]


class FrameSinkConfig(TypedDict, total=False):
    """`FrameSink`'s config: the keys `stream_builder.add` takes."""

    height: int
    """Initial frame height, in pixels.

    Default: `720`.
    """

    scaling: Literal["fit", "fill"]
    """How the frame maps onto the sink.

    Values:
        `fit`: Whole frame visible.
        `fill`: Sink covered.

    Default: `"fit"`.
    """

    title: str
    """Sink title.

    Default: `"StreamLib"`.
    """

    device_id: str | None
    """Paired with `height`."""


class FrameSink(BuiltInNode[FrameSinkConfig]):
    """Shows video frames.

    Inputs:
        `video` (newest): Video frames to show.

    Config: `FrameSinkConfig`.
    """

    type: ClassVar[str] = "tatolab.stream:FrameSink"
"#
    );
    assert_eq!(rendered, golden, "rendered:\n{rendered}");
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

/// A typed built-in renders as a class node is declared: its Stream input and output pins, its
/// settings fields with their types and defaults, and no config class.
///
/// Mental-revert: rendering it as a config class leaves `TextureTinterConfig` in `__all__` and no
/// field on the class.
#[test]
fn a_typed_built_in_renders_its_fields_and_pins_and_no_config() {
    let rendered = render_built_in_node_classes_module(&[descriptor_from(
        texture_tinter_descriptor_with_settings(),
    )])
    .unwrap();
    let golden = format!(
        "{GENERATED_MODULE_HEADER}{}",
        r#"
from typing import Annotated, ClassVar, Literal

from ._built_in_node import BuiltInNode, InputValuesOfATypedBuiltInNode
from ._class_node_pins import InputPin, OutputPin
from ._description import Description
from ._input_declaration_annotations import SettingOnly, StreamDelivery
from ._pin_types import Texture, UInt8

__all__ = [
    "TextureTinter",
]


class TextureTinter(BuiltInNode[InputValuesOfATypedBuiltInNode]):
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
fn a_config_key_with_no_default_is_refused_naming_it() {
    let descriptor = json!({
        "type": "tatolab.stream:Recorder",
        "description": "Records",
        "config_schema": {
            "type": "object",
            "required": ["path"],
            "properties": {"path": {"description": "The file.", "type": "string"}}
        },
        "inputs": [],
        "outputs": [{"name": "done", "description": "Done", "required": true, "delivery_profile": null}]
    });
    let refusal = render_built_in_node_classes_module(&[descriptor_from(descriptor)])
        .unwrap_err()
        .to_string();
    assert!(
        refusal.contains("config key `path` has no default"),
        "{refusal}"
    );
}

#[test]
fn a_test_named_class_opts_out_of_pytest() {
    let descriptor = json!({
        "type": "tatolab.stream:TestRecorder",
        "description": "Records",
        "config_schema": {
            "type": "object",
            "properties": {"path": {"description": "The file.", "type": "string", "default": "recording.bin"}}
        },
        "inputs": [],
        "outputs": [{"name": "done", "description": "Done", "required": true, "delivery_profile": null}]
    });
    let rendered = render_built_in_node_classes_module(&[descriptor_from(descriptor)]).unwrap();
    assert!(
        rendered.contains("class TestRecorderConfig(TypedDict, total=False):"),
        "{rendered}"
    );
    assert!(rendered.contains("        `done`: Done.\n"), "{rendered}");
    assert!(
        rendered.contains("    __test__: ClassVar[bool] = False\n"),
        "{rendered}"
    );
}

/// The catalog lists each config field under `inputs` too, as a setting; the config renders it,
/// so the docstring's inputs name only the ports.
#[test]
fn a_setting_the_catalog_lists_among_the_inputs_is_no_input_of_the_docstring() {
    let descriptor = json!({
        "type": "tatolab.stream:Recorder",
        "description": "Records",
        "config_schema": {
            "type": "object",
            "properties": {"path": {"description": "The file.", "type": "string", "default": "recording.bin"}}
        },
        "inputs": [
            {"name": "audio", "description": "What it records", "required": true,
             "delivery_profile": "ordered"},
            {"name": "path", "description": "The file.", "required": false,
             "delivery_profile": null, "pin_type": {"kind": "is", "pin_type": {"type": "string"}},
             "flow": "value", "input_mode": {"mode": "setting_only", "read": "at_setup"},
             "default_value": "recording.bin"}
        ],
        "outputs": []
    });
    let rendered = render_built_in_node_classes_module(&[descriptor_from(descriptor)]).unwrap();
    assert!(
        rendered.contains("    Inputs:\n        `audio` (ordered): What it records.\n\n"),
        "{rendered}"
    );
    assert!(!rendered.contains("`path`: The file."), "{rendered}");
}

#[test]
fn an_image_outputs_frame_rate_renders_under_the_port_as_a_python_author_writes_it() {
    let descriptor = json!({
        "type": "tatolab.stream:FrameSource",
        "description": "Frames",
        "inputs": [],
        "outputs": [{
            "name": "video",
            "description": "Frames",
            "required": true,
            "delivery_profile": null,
            "family": {
                "family": "image",
                "frame_rate": {"stated": {"numerator": 30000, "denominator": 1001}}
            }
        }, {
            "name": "preview",
            "description": "Preview",
            "required": true,
            "delivery_profile": null,
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
        rendered.contains("Frame rate: `rate=from_config(\"frames_per_second\")`."),
        "{rendered}"
    );
}

#[test]
fn a_description_linking_a_rust_path_is_refused_by_name() {
    let mut descriptor = frame_sink_descriptor_with_every_config_shape();
    descriptor["config_schema"]["properties"]["title"]["description"] =
        json!("Absent means [`FrameSinkScaling::Fit`].");
    let refusal = render_built_in_node_classes_module(&[descriptor_from(descriptor)])
        .unwrap_err()
        .to_string();
    assert!(refusal.contains("`FrameSinkScaling::Fit`"), "{refusal}");
    assert!(refusal.contains("config key `title`"), "{refusal}");
}

#[test]
fn a_blank_description_renders_no_docstring_for_its_key() {
    let mut descriptor = frame_sink_descriptor_with_every_config_shape();
    descriptor["config_schema"]["properties"]["device_id"]["description"] = json!("  ");
    let rendered = render_built_in_node_classes_module(&[descriptor_from(descriptor)]).unwrap();
    assert!(
        rendered.contains("    device_id: str | None\n\n\nclass FrameSink("),
        "{rendered}"
    );
}

#[test]
fn a_json_type_the_generator_does_not_render_is_refused_by_name() {
    let mut descriptor = frame_sink_descriptor_with_every_config_shape();
    descriptor["config_schema"]["properties"]["title"] = json!({"type": "object"});
    let refusal = render_built_in_node_classes_module(&[descriptor_from(descriptor)])
        .unwrap_err()
        .to_string();
    assert!(refusal.contains("`object`"), "{refusal}");
    assert!(
        refusal.contains("`FrameSink`'s config key `title`"),
        "{refusal}"
    );
}

#[test]
fn a_json_array_renders_as_a_list_of_its_items_type() {
    let mut descriptor = frame_sink_descriptor_with_every_config_shape();
    descriptor["config_schema"]["properties"]["device_id"] =
        json!({"type": "array", "items": {"type": "string"}});
    let rendered = render_built_in_node_classes_module(&[descriptor_from(descriptor)]).unwrap();
    assert!(
        rendered.contains("    device_id: list[str]\n"),
        "{rendered}"
    );
}

#[test]
fn a_json_array_with_no_items_schema_is_refused_by_name() {
    let mut descriptor = frame_sink_descriptor_with_every_config_shape();
    descriptor["config_schema"]["properties"]["title"] = json!({"type": "array"});
    let refusal = render_built_in_node_classes_module(&[descriptor_from(descriptor)])
        .unwrap_err()
        .to_string();
    assert!(refusal.contains("no `items` schema"), "{refusal}");
    assert!(
        refusal.contains("`FrameSink`'s config key `title`"),
        "{refusal}"
    );
}

#[test]
fn a_type_outside_the_stream_package_is_refused() {
    let mut descriptor = frame_sink_descriptor_with_every_config_shape();
    descriptor["type"] = json!("streamlib_media_builtins::FrameSink");
    let refusal = render_built_in_node_classes_module(&[descriptor_from(descriptor)])
        .unwrap_err()
        .to_string();
    assert!(refusal.contains("tatolab.stream:"), "{refusal}");
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
        rendered.contains("class Carrier(BuiltInNode[InputValuesOfATypedBuiltInNode]):"),
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
