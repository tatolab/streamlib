// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use serde_json::json;
use streamlib_node_schema::pin_type::BuiltInPinTypeName;
use streamlib_runtime_client_contract::local_api_wire_contract::{
    NodeUiControlPinType, NodeUiManifestControl, NodeUiManifestNodeBody,
};

use super::*;

#[test]
fn a_manifest_reads_its_controls_and_node_bodies_and_a_control_left_unnamed_is_its_types_default() {
    let manifest = read_node_ui_manifest(&json!({
        "contract": "1.1",
        "controls": [
            { "type": "float", "script": "float_number.ts" },
            { "type": "float", "control": "slider", "script": "float_slider.ts" },
            { "type": "vector", "control": "xy_pad", "script": "vector_xy_pad.ts" }
        ],
        "node_bodies": [{ "node_type": "acme:Crossfade", "script": "crossfade_body.ts" }]
    }))
    .unwrap_or_else(|why| panic!("the manifest reads: {why}"));
    assert_eq!(
        manifest.controls,
        vec![
            NodeUiManifestControl {
                pin_type: NodeUiControlPinType::BuiltIn(BuiltInPinTypeName::Float),
                control: None,
                script: "float_number.ts".into(),
            },
            NodeUiManifestControl {
                pin_type: NodeUiControlPinType::BuiltIn(BuiltInPinTypeName::Float),
                control: Some("slider".into()),
                script: "float_slider.ts".into(),
            },
            NodeUiManifestControl {
                pin_type: NodeUiControlPinType::BuiltIn(BuiltInPinTypeName::Vector),
                control: Some("xy_pad".into()),
                script: "vector_xy_pad.ts".into(),
            },
        ]
    );
    assert_eq!(
        manifest.node_bodies,
        vec![NodeUiManifestNodeBody {
            node_type: "acme:Crossfade".into(),
            script: "crossfade_body.ts".into(),
        }]
    );
}

#[test]
fn a_manifest_that_registers_nothing_reads_with_empty_lists() {
    let manifest = read_node_ui_manifest(&json!({ "contract": "1.1" }))
        .unwrap_or_else(|why| panic!("the manifest reads: {why}"));
    assert!(manifest.controls.is_empty());
    assert!(manifest.node_bodies.is_empty());
}

#[test]
fn a_manifest_writes_back_as_it_was_read() {
    let written = json!({
        "contract": "1.1",
        "controls": [
            { "type": "color", "script": "color_well.ts" },
            { "type": "integer", "control": "stepper", "script": "integer_stepper.ts" }
        ],
        "node_bodies": [{ "node_type": "acme:Crossfade", "script": "crossfade_body.ts" }]
    });
    let manifest =
        read_node_ui_manifest(&written).unwrap_or_else(|why| panic!("the manifest reads: {why}"));
    assert_eq!(
        serde_json::to_value(&manifest).unwrap_or_else(|why| panic!("{why}")),
        written
    );
}

#[test]
fn a_control_of_a_pin_type_the_contract_does_not_list_is_refused_at_its_type() {
    let why = read_node_ui_manifest(&json!({
        "contract": "1.1",
        "controls": [
            { "type": "float", "script": "float_number.ts" },
            { "type": "flaot", "script": "float_dial.ts" }
        ]
    }))
    .err()
    .unwrap_or_else(|| panic!("an unknown pin type is refused"));
    assert!(
        why.starts_with("`controls[1].type`: unknown variant `flaot`"),
        "{why}"
    );
}

#[test]
fn a_field_the_manifest_does_not_have_is_refused_by_its_name() {
    for (manifest, field_holder, unknown_field) in [
        (
            json!({ "contract": "1.1", "views": [] }),
            "tatolab_node_ui.json",
            "views",
        ),
        (
            json!({ "contract": "1.1", "controls": [
                { "type": "float", "script": "float_number.ts", "surface": "node" }
            ] }),
            "controls[0]",
            "surface",
        ),
        (
            json!({ "contract": "1.1", "node_bodies": [
                { "node_type": "acme:Crossfade", "script": "crossfade_body.ts", "type": "float" }
            ] }),
            "node_bodies[0]",
            "type",
        ),
    ] {
        let why = read_node_ui_manifest(&manifest)
            .err()
            .unwrap_or_else(|| panic!("`{unknown_field}` is refused"));
        assert!(
            why.starts_with(&format!(
                "`{field_holder}`: unknown field `{unknown_field}`"
            )),
            "{why}"
        );
    }
}

#[test]
fn a_missing_field_and_a_control_written_as_null_are_refused_at_that_field() {
    for (manifest, field_path) in [
        (json!({ "controls": [] }), "contract"),
        (
            json!({ "contract": "1.1", "controls": [{ "type": "float" }] }),
            "controls[0].script",
        ),
        (
            json!({ "contract": "1.1", "controls": [
                { "type": "float", "control": null, "script": "float_number.ts" }
            ] }),
            "controls[0].control",
        ),
        (
            json!({ "contract": "1.1", "node_bodies": [{ "script": "crossfade_body.ts" }] }),
            "node_bodies[0].node_type",
        ),
    ] {
        let why = read_node_ui_manifest(&manifest)
            .err()
            .unwrap_or_else(|| panic!("{manifest} is refused"));
        assert!(why.starts_with(&format!("`{field_path}`: ")), "{why}");
    }
}

#[test]
fn a_contract_this_runtime_does_not_read_node_ui_elements_of_is_refused() {
    for contract in ["2.0", "1.2", "1", "01.1", "one"] {
        let why = read_node_ui_manifest(&json!({ "contract": contract }))
            .err()
            .unwrap_or_else(|| panic!("contract {contract} is refused"));
        assert!(why.starts_with("`contract`: "), "{why}");
    }
    for contract in ["1.0", "1.1"] {
        assert!(read_node_ui_manifest(&json!({ "contract": contract })).is_ok());
    }
}

#[test]
fn a_script_outside_the_node_ui_folder_is_refused_at_that_entry() {
    let why = read_node_ui_manifest(&json!({
        "contract": "1.1",
        "controls": [{ "type": "float", "script": "../float_number.ts" }],
        "node_bodies": [{ "node_type": "acme:Crossfade", "script": "https://example.com/b.ts" }]
    }))
    .err()
    .unwrap_or_else(|| panic!("a script outside the folder is refused"));
    assert!(why.contains("`controls[0].script`: "), "{why}");
    assert!(why.contains("`node_bodies[0].script`: "), "{why}");
}
