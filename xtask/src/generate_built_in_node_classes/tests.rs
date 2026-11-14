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
