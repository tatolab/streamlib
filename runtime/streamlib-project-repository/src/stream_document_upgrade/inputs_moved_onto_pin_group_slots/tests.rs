// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use serde_json::{Value, json};

use crate::stream_document_upgrade::upgrade_stream_document_in_place;

const COMPOSITOR: &str = "tatolab.compositor.alpha_over_compositor:AlphaOverCompositor";

fn upgraded(mut document: Value) -> Value {
    assert!(upgrade_stream_document_in_place(&mut document));
    document
}

fn a_link(source: &str, target: &str, port: &str) -> Value {
    json!({"source": {"node": source, "port": "video"}, "target": {"node": target, "port": port}})
}

#[test]
fn each_wire_into_a_numbered_layer_lands_on_its_slots_picture_and_each_placement_on_its_slots_values()
 {
    let document = json!({
        "nodes": [
            {"id": "compositor-uuid", "name": "compositor", "type": COMPOSITOR, "input_values": {
                "width": 1920,
                "layers": [{}, {"x": 1240, "y": 40, "opacity": 1.0}, {"x": 80, "y": null, "opacity": 0.75}],
            }},
            {"id": "camera-uuid", "name": "camera", "type": "tatolab.camera:CameraSource"},
        ],
        "links": [
            a_link("camera-uuid", "compositor-uuid", "layer_0"),
            a_link("camera-uuid", "compositor", "layer_2"),
            a_link("compositor-uuid", "camera-uuid", "layer_1"),
        ],
    });

    let upgraded = upgraded(document);

    assert_eq!(
        upgraded["nodes"][0]["input_values"],
        json!({
            "width": 1920,
            "layers.1.x": 1240, "layers.1.y": 40, "layers.1.opacity": 1.0,
            "layers.2.x": 80, "layers.2.opacity": 0.75,
        })
    );
    let ports: Vec<&Value> = upgraded["links"]
        .as_array()
        .unwrap()
        .iter()
        .map(|link| &link["target"]["port"])
        .collect();
    assert_eq!(
        ports,
        [
            &json!("layers.0.picture"),
            &json!("layers.2.picture"),
            &json!("layer_1")
        ],
        "a wire into another node keeps its port"
    );
}

#[test]
fn a_slot_value_the_node_already_holds_wins_over_its_list_entry() {
    let upgraded = upgraded(
        json!({"nodes": [{"id": "c", "type": COMPOSITOR, "input_values": {
            "layers.0.x": 5,
            "layers": [{"x": 1, "y": 2}],
        }}]}),
    );

    assert_eq!(
        upgraded["nodes"][0]["input_values"],
        json!({"layers.0.x": 5, "layers.0.y": 2})
    );
}

#[test]
fn an_empty_list_leaves_no_input_values_key() {
    let upgraded = upgraded(
        json!({"nodes": [{"id": "c", "type": COMPOSITOR, "input_values": {"layers": []}}]}),
    );

    assert_eq!(upgraded["nodes"][0], json!({"id": "c", "type": COMPOSITOR}));
}

#[test]
fn a_port_that_only_looks_numbered_and_a_list_holding_no_maps_are_left_as_written() {
    let document = json!({
        "nodes": [{"id": "c", "type": COMPOSITOR, "input_values": {"layers": ["over there"]}}],
        "links": [
            a_link("s", "c", "layer_"),
            a_link("s", "c", "layer_01"),
            a_link("s", "c", "layer_-1"),
            a_link("s", "c", "video"),
        ],
    });
    let mut upgraded = document.clone();

    assert!(!upgrade_stream_document_in_place(&mut upgraded));
    assert_eq!(upgraded, document);
}

#[test]
fn another_node_type_with_numbered_inputs_is_left_alone() {
    let document = json!({
        "nodes": [{"id": "m", "type": "project_nodes:Mixer", "input_values": {"layers": [{"x": 1}]}}],
        "links": [a_link("s", "m", "layer_0")],
    });
    let mut upgraded = document.clone();

    assert!(!upgrade_stream_document_in_place(&mut upgraded));
    assert_eq!(upgraded, document);
}

#[test]
fn a_compositor_inside_a_fragment_moves_onto_its_slots_and_a_second_upgrade_finds_nothing() {
    let document = json!({"nodes": [{"id": "f", "type": "fragment", "fragment": {
        "nodes": [{"id": "c", "type": COMPOSITOR, "input_values": {"layers": [{"opacity": 0.5}]}}],
        "links": [a_link("s", "c", "layer_9")],
    }}]});

    let mut upgraded = upgraded(document);

    let fragment = &upgraded["nodes"][0]["fragment"];
    assert_eq!(
        fragment["nodes"][0]["input_values"],
        json!({"layers.0.opacity": 0.5})
    );
    assert_eq!(
        fragment["links"][0]["target"]["port"],
        json!("layers.9.picture")
    );
    assert!(!upgrade_stream_document_in_place(&mut upgraded));
}
