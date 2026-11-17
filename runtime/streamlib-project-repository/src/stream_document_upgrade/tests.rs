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
    assert_eq!(RENAMED_NODE_TYPES.len(), 30);
    for (from, to) in RENAMED_NODE_TYPES {
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
