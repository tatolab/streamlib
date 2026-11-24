// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use serde_json::json;

/// `document` upgraded, `None` when it is already in the current shape.
fn upgraded_stream_document(document: &Value) -> Option<Value> {
    let mut upgraded = document.clone();
    upgrade_stream_document_in_place(&mut upgraded).then_some(upgraded)
}

#[test]
fn a_document_already_in_the_current_shape_needs_no_upgrade() {
    let document = json!({
        "nodes": [{
            "id": "00000000-0000-0000-0000-000000000001",
            "name": "tone",
            "type": "nodes.tone:Tone",
            "input_values": {"pitch": 440.0},
            "exposed_config_fields": ["pitch"]
        }],
        "links": []
    });
    assert_eq!(upgraded_stream_document(&document), None);
    assert_eq!(upgraded_stream_document(&json!({"links": []})), None);
}

#[test]
fn config_and_pins_fold_into_input_values_with_a_pin_over_a_config_field() {
    let document = json!({
        "nodes": [{
            "id": "00000000-0000-0000-0000-000000000001",
            "name": "tone",
            "type": "nodes.tone:Tone",
            "config": {"waveform": "sine", "pitch": 220.0, "unset": null},
            "pins": {"pitch": 440.0},
            "exposed_config_fields": ["pitch"],
            "studio": {"position": {"x": 1.0, "y": 2.0}}
        }]
    });
    assert_eq!(
        upgraded_stream_document(&document),
        Some(json!({
            "nodes": [{
                "id": "00000000-0000-0000-0000-000000000001",
                "name": "tone",
                "type": "nodes.tone:Tone",
                "input_values": {"waveform": "sine", "pitch": 440.0},
                "exposed_config_fields": ["pitch"],
                "studio": {"position": {"x": 1.0, "y": 2.0}}
            }]
        }))
    );
}

/// A `graph` render of a `GraphInput` whose value was cleared holds no `value` in its input values
/// and the Studio's `default` mirror in its config from before; read back, it holds no value. A
/// document written before `value` was an input, holding only a `config` `default`, keeps it.
///
/// Mental-revert: folding the rendered `default` gives the cleared `GraphInput` its old value
/// back, as its former name.
#[test]
fn a_rendered_graph_inputs_default_is_not_folded_and_a_legacy_one_is() {
    let node = |fields: Value| {
        let mut node = json!({
            "id": "00000000-0000-0000-0000-000000000001",
            "name": "warmth_base",
            "type": "tatolab.stream:GraphInput",
        });
        node.as_object_mut()
            .unwrap()
            .extend(fields.as_object().unwrap().clone());
        json!({ "nodes": [node], "links": [] })
    };
    let rendered = upgraded_stream_document(&node(json!({
        "input_values": { "port": "warmth_base" },
        "config": { "port": "warmth_base", "default": 0.1 },
    })))
    .expect("the render's config folds");
    assert_eq!(
        rendered["nodes"][0]["input_values"],
        json!({ "port": "warmth_base" })
    );
    let legacy = upgraded_stream_document(&node(json!({
        "config": { "port": "warmth_base", "default": 0.1 },
    })))
    .expect("the legacy config folds");
    assert_eq!(
        legacy["nodes"][0]["input_values"],
        json!({ "port": "warmth_base", "default": 0.1 })
    );
}

#[test]
fn a_name_input_values_already_holds_wins_over_both_old_fields() {
    let document = json!({
        "nodes": [{
            "name": "tone",
            "type": "nodes.tone:Tone",
            "input_values": {"pitch": 880.0},
            "config": {"pitch": 220.0, "waveform": "saw"},
            "pins": {"pitch": 440.0}
        }]
    });
    assert_eq!(
        upgraded_stream_document(&document).expect("an upgrade")["nodes"][0],
        json!({
            "name": "tone",
            "type": "nodes.tone:Tone",
            "input_values": {"pitch": 880.0, "waveform": "saw"}
        })
    );
}

#[test]
fn an_empty_config_folds_to_no_input_values_key() {
    let document = json!({"nodes": [{"name": "a", "type": "nodes.a:A", "config": {}}]});
    assert_eq!(
        upgraded_stream_document(&document),
        Some(json!({"nodes": [{"name": "a", "type": "nodes.a:A"}]}))
    );
}

#[test]
fn the_fold_reaches_every_level_of_a_fragment() {
    let document = json!({
        "nodes": [{
            "name": "outer",
            "type": "generators.blur:Blur",
            "config": {"radius": 3},
            "fragment": {
                "nodes": [{
                    "name": "middle",
                    "type": "generators.inner:Inner",
                    "config": {"depth": 1},
                    "fragment": {
                        "nodes": [{
                            "name": "lfo",
                            "type": "tatolab.ops:Lfo",
                            "pins": {"rate_hz": 0.5}
                        }],
                        "links": []
                    }
                }],
                "links": []
            }
        }]
    });
    assert_eq!(
        upgraded_stream_document(&document),
        Some(json!({
            "nodes": [{
                "name": "outer",
                "type": "generators.blur:Blur",
                "input_values": {"radius": 3},
                "fragment": {
                    "nodes": [{
                        "name": "middle",
                        "type": "generators.inner:Inner",
                        "input_values": {"depth": 1},
                        "fragment": {
                            "nodes": [{
                                "name": "lfo",
                                "type": "imports/core-nodes/core_nodes.animation:Lfo",
                                "input_values": {"rate_hz": 0.5}
                            }],
                            "links": []
                        }
                    }],
                    "links": []
                }
            }]
        }))
    );
}

#[test]
fn each_renamed_node_type_takes_its_new_type_and_keeps_everything_else() {
    assert_eq!(RENAMED_NODE_TYPES.len(), 35);
    for (from, to) in RENAMED_NODE_TYPES
        .iter()
        .filter(|(_, to)| *to != CONSTANT_NODE_TYPE)
    {
        let document = json!({
            "nodes": [{
                "id": "00000000-0000-0000-0000-000000000001",
                "name": "node",
                "type": from,
                "input_values": {"rate_hz": 0.5},
                "studio": {"position": {"x": 10.0, "y": 20.0}}
            }],
            "links": [{
                "source": {"node": "00000000-0000-0000-0000-000000000001", "port": "out"},
                "target": {"node": "00000000-0000-0000-0000-000000000002", "port": "in"}
            }]
        });
        let mut expected = document.clone();
        expected["nodes"][0]["type"] = json!(to);
        assert_eq!(
            upgraded_stream_document(&document),
            Some(expected),
            "{from}"
        );
    }
}

#[test]
fn a_constant_that_held_one_float_becomes_the_runtimes_own_holding_that_float() {
    for (from, saved, held) in [
        ("tatolab.ops:Constant", json!({"value": 0.25}), json!(0.25)),
        (
            "imports/core-nodes/core_nodes.math_nodes:Constant",
            json!({"value": 2}),
            json!(2),
        ),
        (
            "imports/core-nodes/core_nodes.math_nodes:Constant",
            json!({}),
            json!(0.0),
        ),
    ] {
        let document = json!({
            "nodes": [{
                "id": "00000000-0000-0000-0000-000000000001",
                "name": "level",
                "type": from,
                "input_values": saved,
                "studio": {"position": {"x": 10.0, "y": 20.0}}
            }],
            "links": [{
                "source": {"node": "00000000-0000-0000-0000-000000000001", "port": "result"},
                "target": {"node": "00000000-0000-0000-0000-000000000002", "port": "in"}
            }]
        });
        let mut expected = document.clone();
        expected["nodes"][0]["type"] = json!(CONSTANT_NODE_TYPE);
        expected["nodes"][0]["input_values"] =
            json!({"pin_type": {"type": "float"}, "value": held});

        let upgraded = upgraded_stream_document(&document).expect("an upgrade");

        assert_eq!(upgraded, expected, "{from} saving {saved}");
        assert_eq!(
            upgraded_stream_document(&upgraded),
            None,
            "{from} saving {saved}"
        );
    }
}

#[test]
fn a_constant_of_the_runtimes_own_type_is_left_as_it_is() {
    let document = json!({
        "nodes": [{
            "id": "00000000-0000-0000-0000-000000000001",
            "name": "word",
            "type": CONSTANT_NODE_TYPE,
            "input_values": {"pin_type": {"type": "string"}, "value": "hello"}
        }],
        "links": []
    });
    assert_eq!(upgraded_stream_document(&document), None);
}

#[test]
fn each_built_in_conversion_takes_the_name_of_the_pin_types_it_converts_between() {
    for (old_type, new_type, input_port, output_port, config) in [
        (
            "tatolab.stream:ImageToArray",
            "tatolab.stream:TextureToTensor",
            "image",
            "array",
            json!({}),
        ),
        (
            "tatolab.stream:ArrayToImage",
            "tatolab.stream:TensorToTexture",
            "array",
            "image",
            json!({"attribute": "image", "format": "bgra8_unorm"}),
        ),
        (
            "tatolab.stream:ChannelsToArray",
            "tatolab.stream:AudioToTensor",
            "channels",
            "array",
            json!({}),
        ),
        (
            "tatolab.stream:ArrayToChannels",
            "tatolab.stream:TensorToAudio",
            "array",
            "channels",
            json!({"channel_names": ["left", "right"], "sample_rate": 44100}),
        ),
    ] {
        let document = json!({
            "nodes": [{
                "id": "00000000-0000-0000-0000-000000000001",
                "name": "convert",
                "type": old_type,
                "config": config,
                "studio": {"position": {"x": 10.0, "y": 20.0}}
            }],
            "links": [
                {
                    "source": {"node": "00000000-0000-0000-0000-000000000002", "port": "out"},
                    "target": {"node": "00000000-0000-0000-0000-000000000001", "port": input_port}
                },
                {
                    "source": {"node": "00000000-0000-0000-0000-000000000001", "port": output_port},
                    "target": {"node": "00000000-0000-0000-0000-000000000003", "port": "in"}
                }
            ]
        });
        let mut expected = document.clone();
        expected["nodes"][0] = json!({
            "id": "00000000-0000-0000-0000-000000000001",
            "name": "convert",
            "type": new_type,
            "studio": {"position": {"x": 10.0, "y": 20.0}}
        });
        if config.as_object().is_some_and(|config| !config.is_empty()) {
            expected["nodes"][0]["input_values"] = config;
        }

        let upgraded = upgraded_stream_document(&document).expect("an upgrade");

        assert_eq!(upgraded, expected, "{old_type}");
        assert_eq!(upgraded_stream_document(&upgraded), None, "{old_type}");
    }
}

#[test]
fn a_renamed_type_is_never_a_type_another_row_renames() {
    for (_, to) in RENAMED_NODE_TYPES {
        assert_eq!(the_type_a_node_type_was_renamed_to(to), None, "{to}");
    }
}

#[test]
fn a_type_never_renamed_is_left_alone() {
    for node_type in [
        "tatolab.ops:NoSuchNode",
        "tatolab.opsy:Lfo",
        "tatolab.ops.other:Lfo",
        "imports/core-nodes/core_nodes.animation:Lfo",
        "tatolab.stream:PreviousValue",
        "nodes.tone:Tone",
    ] {
        assert_eq!(the_type_a_node_type_was_renamed_to(node_type), None);
    }
}

#[test]
fn a_node_holding_an_old_field_that_is_no_object_is_left_as_written() {
    let document = json!({"nodes": [{"name": "a", "type": "nodes.a:A", "config": [1]}]});
    assert_eq!(upgraded_stream_document(&document), None);
}

#[test]
fn the_upgrade_is_deterministic_and_an_upgraded_document_needs_no_second() {
    let document = json!({
        "nodes": [{"name": "a", "type": "tatolab.ops:Add", "config": {"b": 1}, "pins": {"a": 2}}]
    });
    let once = upgraded_stream_document(&document).expect("an upgrade");
    assert_eq!(upgraded_stream_document(&document), Some(once.clone()));
    assert_eq!(upgraded_stream_document(&once), None);
}
