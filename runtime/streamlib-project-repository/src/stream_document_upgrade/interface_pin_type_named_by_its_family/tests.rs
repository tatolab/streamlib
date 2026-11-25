// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use serde_json::{Value, json};

use super::the_family_of_an_interface_node_chosen;

fn upgraded(node: Value) -> (bool, Value) {
    let mut node = node.as_object().cloned().expect("a node is an object");
    let changed = the_family_of_an_interface_node_chosen(&mut node);
    (changed, Value::Object(node))
}

#[test]
fn an_image_or_array_interface_node_chooses_the_type_its_family_implied() {
    for (family, pin_type) in [
        ("image", json!({"type": "texture"})),
        ("array", json!({"type": "tensor"})),
    ] {
        let (changed, node) = upgraded(json!({
            "type": "tatolab.stream:GraphOutput",
            "input_values": {"port": "out", "family": family},
        }));
        assert!(changed);
        assert_eq!(
            node["input_values"],
            json!({"port": "out", "pin_type": pin_type})
        );
    }
}

#[test]
fn a_channels_graph_input_chooses_the_type_of_the_value_it_saved() {
    for (saved, pin_type) in [
        (json!(0.5), json!({"type": "float"})),
        (json!(true), json!({"type": "boolean"})),
    ] {
        let (_, node) = upgraded(json!({
            "type": "tatolab.stream:GraphInput",
            "input_values": {"port": "level", "family": "channels", "value": saved},
        }));
        assert_eq!(node["input_values"]["pin_type"], pin_type);
        assert!(node["input_values"].get("family").is_none());
    }
}

#[test]
fn a_family_that_implied_no_one_type_is_dropped_so_its_first_link_types_the_port() {
    for family in ["records", "packets", "channels"] {
        let (changed, node) = upgraded(json!({
            "type": "tatolab.stream:GraphInput",
            "input_values": {"port": "in", "family": family},
        }));
        assert!(changed);
        assert_eq!(node["input_values"], json!({"port": "in"}));
    }
}

#[test]
fn a_chosen_pin_type_is_kept_and_any_other_node_is_left_as_written() {
    let (_, chosen) = upgraded(json!({
        "type": "tatolab.stream:GraphInput",
        "input_values": {"port": "in", "family": "image", "pin_type": {"type": "string"}},
    }));
    assert_eq!(
        chosen["input_values"],
        json!({"port": "in", "pin_type": {"type": "string"}})
    );
    let other = json!({"type": "tatolab_codecs:Encoder", "input_values": {"family": "image"}});
    assert_eq!(upgraded(other.clone()), (false, other));
}
