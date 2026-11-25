// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use serde_json::{Value, json};

use crate::upgrade_stream_document_in_place;

const SET_PULSE: &str = "3c0e6b2a-9d4f-4a1e-8b7c-5f2d1e0a9b84";
const SET_GLOW: &str = "7d1f4e9a-2b3c-4d5e-9f8a-1b2c3d4e5f60";

fn upgraded(mut document: Value) -> (bool, Value) {
    let changed = upgrade_stream_document_in_place(&mut document);
    (changed, document)
}

fn a_level_whose_gets_name_their_variables() -> Value {
    json!({
        "nodes": [
            {"id": SET_PULSE, "name": "set-pulse", "type": "tatolab.stream:SetVariable",
             "input_values": {"variable": "pulse"}},
            {"id": SET_GLOW, "name": "set-glow", "type": "tatolab.stream:SetVariable",
             "input_values": {"variable": "glow"}},
            {"name": "get-pulse", "type": "tatolab.stream:GetVariable",
             "input_values": {"variable": "pulse"}},
            {"name": "get-outer", "type": "tatolab.stream:GetVariable",
             "input_values": {"variable": "outer"}}
        ]
    })
}

#[test]
fn a_get_naming_a_variable_its_level_sets_names_that_setter_by_uuid() {
    let (changed, document) = upgraded(a_level_whose_gets_name_their_variables());
    assert!(changed);
    assert_eq!(
        document["nodes"][2]["input_values"],
        json!({ "set_variable": SET_PULSE })
    );
}

#[test]
fn a_get_naming_a_variable_no_setter_of_its_level_sets_keeps_it() {
    let (_, document) = upgraded(a_level_whose_gets_name_their_variables());
    assert_eq!(
        document["nodes"][3]["input_values"],
        json!({ "variable": "outer" })
    );
}

#[test]
fn a_get_already_naming_its_setter_needs_no_upgrade() {
    let (_, once) = upgraded(a_level_whose_gets_name_their_variables());
    let (changed, twice) = upgraded(once.clone());
    assert!(!changed);
    assert_eq!(once, twice);
}

#[test]
fn a_get_inside_a_fragment_names_the_setter_of_the_fragments_level() {
    let (_, document) = upgraded(json!({
        "nodes": [{
            "name": "look",
            "type": "tatolab.project:StreamFragment",
            "fragment": a_level_whose_gets_name_their_variables()
        }]
    }));
    assert_eq!(
        document["nodes"][0]["fragment"]["nodes"][2]["input_values"],
        json!({ "set_variable": SET_PULSE })
    );
}

#[test]
fn a_get_naming_a_variable_two_setters_of_its_level_set_keeps_it() {
    let mut level = a_level_whose_gets_name_their_variables();
    level["nodes"][1]["input_values"]["variable"] = json!("pulse");
    let (_, document) = upgraded(level);
    assert_eq!(
        document["nodes"][2]["input_values"],
        json!({ "variable": "pulse" })
    );
}

#[test]
fn a_get_whose_setter_reference_is_null_names_its_variables_setter() {
    let (changed, document) = upgraded(json!({
        "nodes": [
            {"id": SET_PULSE, "name": "set-pulse", "type": "tatolab.stream:SetVariable",
             "input_values": {"variable": "pulse"}},
            {"name": "get-pulse", "type": "tatolab.stream:GetVariable",
             "input_values": {"set_variable": null, "variable": "pulse"}}
        ]
    }));
    assert!(changed);
    assert_eq!(
        document["nodes"][1]["input_values"],
        json!({ "set_variable": SET_PULSE })
    );
}

#[test]
fn a_get_of_a_setter_whose_id_is_no_uuid_keeps_its_variable() {
    let (_, document) = upgraded(json!({
        "nodes": [
            {"id": "set-pulse", "name": "set-pulse", "type": "tatolab.stream:SetVariable",
             "input_values": {"variable": "pulse"}},
            {"name": "get-pulse", "type": "tatolab.stream:GetVariable",
             "input_values": {"variable": "pulse"}}
        ]
    }));
    assert_eq!(
        document["nodes"][1]["input_values"],
        json!({ "variable": "pulse" })
    );
}
