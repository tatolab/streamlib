// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::super::{upgrade_a_node_with, upgrade_the_document_with};
use super::*;

const TINT: &str = "00000000-0000-0000-0000-000000000001";
const LFO: &str = "00000000-0000-0000-0000-000000000002";

/// A Color whose red, green and blue were three numbers, with alpha always 0.5.
const TINT_COLOR: MergedInput = MergedInput {
    node_type: "nodes.tint:Tint",
    input: "color",
    merged_into: MergedInto::Color,
    parts: &[
        MergedPart::FedBy {
            old_input: "red",
            default: 1.0,
        },
        MergedPart::FedBy {
            old_input: "green",
            default: 0.25,
        },
        MergedPart::FedBy {
            old_input: "blue",
            default: 0.0,
        },
        MergedPart::Fixed(0.5),
    ],
};

/// A Vector of two that was two numbers.
const TINT_OFFSET: MergedInput = MergedInput {
    node_type: "nodes.tint:Tint",
    input: "offset",
    merged_into: MergedInto::Vector,
    parts: &[
        MergedPart::FedBy {
            old_input: "offset_x",
            default: 0.0,
        },
        MergedPart::FedBy {
            old_input: "offset_y",
            default: 0.0,
        },
    ],
};

const ROWS: &[MergedInput] = &[TINT_COLOR, TINT_OFFSET];

/// `document` upgraded with the tests' rows, `None` when nothing changed.
fn upgraded(document: &Value) -> Option<Value> {
    let mut upgraded = document.clone();
    upgrade_the_document_with(&mut upgraded, ROWS).then_some(upgraded)
}

fn a_tint_holding(input_values: Value) -> Value {
    json!({
        "id": TINT,
        "name": "tint",
        "type": "nodes.tint:Tint",
        "input_values": input_values,
        "studio": {"position": {"x": 400.0, "y": 100.0}}
    })
}

fn an_lfo() -> Value {
    json!({"id": LFO, "name": "lfo", "type": "nodes.lfo:Lfo"})
}

fn a_link(from: (&str, &str), to: (&str, &str)) -> Value {
    json!({
        "source": {"node": from.0, "port": from.1},
        "target": {"node": to.0, "port": to.1}
    })
}

fn the_combining_node_of(document: &Value, input: &str) -> Value {
    let id = the_combining_node_id(TINT, input);
    document["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|node| node["id"] == id)
        .cloned()
        .unwrap_or_else(|| panic!("no combining node feeds `{input}`"))
}

#[test]
fn every_part_saved_opens_as_the_one_color_with_the_fixed_part_written() {
    let document = json!({
        "nodes": [a_tint_holding(json!({"red": 0.1, "green": 0.2, "blue": 0.3, "gain": 2.0}))],
        "links": []
    });
    let upgraded = upgraded(&document).expect("the three numbers merge");
    assert_eq!(
        upgraded["nodes"][0]["input_values"],
        json!({
            "color": {"red": 0.1, "green": 0.2, "blue": 0.3, "alpha": 0.5},
            "gain": 2.0
        })
    );
    assert_eq!(upgraded["nodes"].as_array().unwrap().len(), 1);
    assert_eq!(upgraded["links"], json!([]));
}

#[test]
fn a_part_left_unsaved_takes_the_rows_default_so_the_value_is_written_whole() {
    let document = json!({
        "nodes": [a_tint_holding(json!({"green": 0.75, "offset_y": 3.0}))]
    });
    let upgraded = upgraded(&document).expect("the saved parts merge");
    assert_eq!(
        upgraded["nodes"][0]["input_values"],
        json!({
            "color": {"red": 1.0, "green": 0.75, "blue": 0.0, "alpha": 0.5},
            "offset": {"x": 0.0, "y": 3.0}
        })
    );
    assert_eq!(upgraded.get("links"), None);
}

#[test]
fn a_node_with_no_old_input_saved_or_wired_is_left_as_written_and_its_input_unset() {
    let document = json!({
        "nodes": [a_tint_holding(json!({"gain": 2.0})), an_lfo()],
        "links": [a_link((LFO, "value"), (TINT, "gain"))]
    });
    assert_eq!(upgraded(&document), None);
}

#[test]
fn a_wired_part_is_kept_through_a_combining_node_holding_the_other_parts() {
    let document = json!({
        "nodes": [a_tint_holding(json!({"gain": 2.0})), an_lfo()],
        "links": [a_link((LFO, "value"), (TINT, "red"))]
    });
    let upgraded = upgraded(&document).expect("the wired part merges");
    let combining_node = the_combining_node_of(&upgraded, "color");
    let combining_id = the_combining_node_id(TINT, "color");
    assert_eq!(
        combining_node,
        json!({
            "id": combining_id,
            "name": "tint_color",
            "type": COMBINE_COLOR_NODE_TYPE,
            "input_values": {"green": 0.25, "blue": 0.0, "alpha": 0.5},
            "studio": {"position": {"x": 160.0, "y": 100.0}}
        })
    );
    assert_eq!(upgraded["nodes"][0]["input_values"], json!({"gain": 2.0}));
    assert_eq!(
        upgraded["links"],
        json!([
            a_link((LFO, "value"), (&combining_id, "red")),
            a_link((&combining_id, COMBINED_OUTPUT), (TINT, "color")),
        ])
    );
}

#[test]
fn a_wired_part_wins_over_its_saved_number_and_the_other_saved_parts_go_to_the_combining_node() {
    let document = json!({
        "nodes": [a_tint_holding(json!({"red": 0.9, "blue": 0.4})), an_lfo()],
        "links": [a_link((LFO, "value"), (TINT, "red"))]
    });
    let upgraded = upgraded(&document).expect("the wired part merges");
    assert_eq!(
        the_combining_node_of(&upgraded, "color")["input_values"],
        json!({"green": 0.25, "blue": 0.4, "alpha": 0.5})
    );
    assert_eq!(upgraded["nodes"][0].get("input_values"), None);
}

#[test]
fn a_wired_vector_part_is_kept_through_a_combining_vector_of_the_rows_parts() {
    let document = json!({
        "nodes": [a_tint_holding(json!({"offset_y": 3.0})), an_lfo()],
        "links": [a_link((LFO, "value"), (TINT, "offset_x"))]
    });
    let upgraded = upgraded(&document).expect("the wired part merges");
    let combining_node = the_combining_node_of(&upgraded, "offset");
    let combining_id = the_combining_node_id(TINT, "offset");
    assert_eq!(combining_node["type"], COMBINE_VECTOR_NODE_TYPE);
    assert_eq!(combining_node["input_values"], json!({"y": 3.0}));
    assert_eq!(
        combining_node["studio"],
        json!({"position": {"x": 160.0, "y": 220.0}})
    );
    assert_eq!(
        upgraded["links"],
        json!([
            a_link((LFO, "value"), (&combining_id, "x")),
            a_link((&combining_id, COMBINED_OUTPUT), (TINT, "offset")),
        ])
    );
}

#[test]
fn upgrading_twice_gives_what_upgrading_once_does() {
    for document in [
        json!({"nodes": [a_tint_holding(json!({"red": 0.1, "offset_x": 1.0}))], "links": []}),
        json!({
            "nodes": [a_tint_holding(json!({"blue": 0.3})), an_lfo()],
            "links": [
                a_link((LFO, "value"), (TINT, "green")),
                a_link((LFO, "value"), (TINT, "offset_y")),
            ]
        }),
    ] {
        let once = upgraded(&document).expect("the first upgrade merges");
        assert_eq!(upgraded(&once), None, "{once:#}");
    }
}

#[test]
fn a_node_already_holding_the_merged_input_keeps_its_old_inputs_as_written() {
    let document = json!({
        "nodes": [a_tint_holding(json!({
            "color": {"red": 0.0, "green": 0.0, "blue": 0.0, "alpha": 1.0},
            "red": 0.7
        }))]
    });
    assert_eq!(upgraded(&document), None);
}

#[test]
fn a_combining_node_takes_a_name_no_node_of_its_level_holds() {
    let mut taken = an_lfo();
    taken["name"] = json!("tint_color");
    let document = json!({
        "nodes": [a_tint_holding(json!({})), taken],
        "links": [a_link((LFO, "value"), (TINT, "blue"))]
    });
    let upgraded = upgraded(&document).expect("the wired part merges");
    assert_eq!(
        the_combining_node_of(&upgraded, "color")["name"],
        "tint_color_2"
    );
}

#[test]
fn the_inputs_of_a_fragment_merge_with_the_fragments_own_links_after_its_pins_fold() {
    let mut tint = a_tint_holding(json!({}));
    tint["type"] = json!("nodes.tint:Tint");
    tint.as_object_mut().unwrap().remove("input_values");
    tint["pins"] = json!({"red": 0.2});
    let document = json!({
        "nodes": [{
            "id": "00000000-0000-0000-0000-0000000000f0",
            "name": "look",
            "type": "nodes.look:Look",
            "fragment": {
                "nodes": [tint, an_lfo()],
                "links": [a_link((LFO, "value"), (TINT, "green"))]
            }
        }],
        "links": []
    });
    let upgraded = upgraded(&document).expect("the fragment's inputs merge");
    let fragment = &upgraded["nodes"][0]["fragment"];
    assert_eq!(
        the_combining_node_of(fragment, "color")["input_values"],
        json!({"red": 0.2, "blue": 0.0, "alpha": 0.5})
    );
    assert_eq!(fragment["links"].as_array().unwrap().len(), 2);
    assert_eq!(upgraded["links"], json!([]));
}

#[test]
fn a_node_placed_on_its_own_takes_the_merged_value_of_what_it_saved() {
    let mut node = a_tint_holding(json!({"red": 0.1}));
    assert!(upgrade_a_node_with(&mut node, ROWS));
    assert_eq!(
        node["input_values"],
        json!({"color": {"red": 0.1, "green": 0.25, "blue": 0.0, "alpha": 0.5}})
    );
    assert!(!upgrade_a_node_with(&mut node, ROWS));
}

const CORE_TINT: &str = "imports/core-nodes/core_nodes.color:Tint";
const CORE_DUOTONE: &str = "imports/core-nodes/core_nodes.color:Duotone";

fn a_core_node(node_type: &str, input_values: Value) -> Value {
    json!({
        "id": TINT,
        "name": "look",
        "type": node_type,
        "input_values": input_values,
        "studio": {"position": {"x": 400.0, "y": 100.0}}
    })
}

/// `document` upgraded with the shipped rows, as a runtime reads it.
fn upgraded_as_shipped(document: &Value) -> Option<Value> {
    let mut upgraded = document.clone();
    super::super::upgrade_stream_document_in_place(&mut upgraded).then_some(upgraded)
}

#[test]
fn a_core_tint_saved_with_some_numbers_opens_as_one_color_of_its_old_defaults_and_opaque() {
    let document = json!({
        "nodes": [a_core_node(CORE_TINT, json!({"green": 0.9, "amount": 0.5}))],
        "links": []
    });
    let upgraded = upgraded_as_shipped(&document).expect("Tint's numbers merge");
    assert_eq!(
        upgraded["nodes"][0]["input_values"],
        json!({
            "color": {"red": 1.0, "green": 0.9, "blue": 0.2, "alpha": 1.0},
            "amount": 0.5
        })
    );
}

#[test]
fn a_core_duotone_opens_with_its_dark_and_light_colors_and_a_wire_kept_on_a_combine_color() {
    let document = json!({
        "nodes": [
            a_core_node(CORE_DUOTONE, json!({"dark_blue": 0.6, "light_red": 0.7})),
            an_lfo()
        ],
        "links": [a_link((LFO, "value"), (TINT, "light_green"))]
    });
    let upgraded = upgraded_as_shipped(&document).expect("Duotone's numbers merge");
    assert_eq!(
        upgraded["nodes"][0]["input_values"],
        json!({"dark": {"red": 0.05, "green": 0.0, "blue": 0.6, "alpha": 1.0}})
    );
    let combining_id = the_combining_node_id(TINT, "light");
    let combining_node = the_combining_node_of(&upgraded, "light");
    assert_eq!(combining_node["type"], COMBINE_COLOR_NODE_TYPE);
    assert_eq!(
        combining_node["input_values"],
        json!({"red": 0.7, "blue": 0.4, "alpha": 1.0})
    );
    assert_eq!(
        upgraded["links"],
        json!([
            a_link((LFO, "value"), (&combining_id, "green")),
            a_link((&combining_id, COMBINED_OUTPUT), (TINT, "light")),
        ])
    );
    assert_eq!(upgraded_as_shipped(&upgraded), None);
}

#[test]
fn a_core_tint_or_duotone_saved_with_no_numbers_is_left_as_written() {
    for node_type in [CORE_TINT, CORE_DUOTONE] {
        let document = json!({"nodes": [a_core_node(node_type, json!({}))]});
        assert_eq!(upgraded_as_shipped(&document), None, "{node_type}");
    }
}
