// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::path::PathBuf;

use serde_json::json;
use streamlib_project_repository::{
    ProjectBringingAView, ProjectViewFolderRead, read_tatolab_view_manifest,
};

use super::*;

fn a_view_read_from(view_type: &str, manifest_json: JsonValue) -> ProjectViewFolderRead {
    ProjectViewFolderRead {
        view_type: view_type.to_string(),
        brought_by: ProjectBringingAView {
            project: uuid::Uuid::nil(),
            project_name: "acme-dj".to_string(),
            import_folder: None,
        },
        folder: PathBuf::from("/acme-dj/views/deck"),
        manifest: read_tatolab_view_manifest(&manifest_json),
        manifest_as_written: Some(manifest_json),
    }
}

fn the_deck_manifest() -> JsonValue {
    json!({
        "contract": "1.0", "title": "DJ Deck", "description": "Two turntables and a crossfader.",
        "entry": "index.html", "placement": "node",
        "inputs": [{ "name": "waveform", "family": "channels", "description": "What plays." }],
        "outputs": [{ "name": "crossfader", "family": "array", "range": [-1, 1],
                      "description": "Left to right." }],
        "settings": { "type": "object",
                      "properties": { "bpm": { "type": "number", "default": 120 } } }
    })
}

#[test]
fn a_view_node_takes_only_the_settings_its_view_declares_unless_the_view_opens_them() {
    let config_schema_of = |manifest: JsonValue| {
        the_node_type_of_a_project_view(&a_view_read_from("acme-dj:deck", manifest))
            .expect("a node view that opens is a node type")
            .config_schema
            .expect("a view node publishes a config schema")
    };
    let mut no_settings = the_deck_manifest();
    no_settings.as_object_mut().unwrap().remove("settings");
    assert_eq!(
        config_schema_of(no_settings),
        json!({ "type": "object", "properties": {}, "additionalProperties": false })
    );
    let mut open_settings = the_deck_manifest();
    open_settings["settings"]["additionalProperties"] = json!(true);
    assert_eq!(
        config_schema_of(open_settings)["additionalProperties"],
        json!(true)
    );
}

#[test]
fn a_node_view_is_a_node_type_of_its_declared_ports_description_and_settings() {
    let descriptor =
        the_node_type_of_a_project_view(&a_view_read_from("acme-dj:deck", the_deck_manifest()))
            .expect("a node view that opens is a node type");

    assert_eq!(descriptor.node_class_import_path.as_str(), "acme-dj:deck");
    assert_eq!(descriptor.node_class_short_name.as_str(), "DJ Deck");
    assert_eq!(descriptor.description, "Two turntables and a crossfader.");
    assert_eq!(
        descriptor.config_schema,
        Some(json!({ "type": "object",
                     "properties": { "bpm": { "type": "number", "default": 120 } },
                     "additionalProperties": false }))
    );
    let ports = |ports: &[PortDescriptor]| -> Vec<(String, String, Vec<PortFamily>, bool)> {
        ports
            .iter()
            .map(|port| {
                (
                    port.name.clone(),
                    port.description.clone(),
                    port.family.families_an_input_takes(),
                    port.required,
                )
            })
            .collect()
    };
    assert_eq!(
        ports(&descriptor.inputs),
        [
            (
                "waveform".to_string(),
                "What plays.".to_string(),
                vec![PortFamily::Channels],
                false
            ),
            (
                "crossfader".to_string(),
                "Left to right.".to_string(),
                vec![PortFamily::Array],
                false
            )
        ]
    );
    assert_eq!(
        descriptor.inputs[0].delivery_profile.as_deref(),
        Some("newest"),
        "a link into a view node's input is wired as a channel, which takes a delivery profile"
    );
    assert_eq!(
        descriptor.inputs[0].input_mode, None,
        "a declared input takes a wire"
    );
    assert_eq!(
        ports(&descriptor.outputs),
        [(
            "crossfader".to_string(),
            "Left to right.".to_string(),
            vec![PortFamily::Array],
            false
        )]
    );
}

#[test]
fn each_output_a_panel_writes_is_a_value_publishing_its_same_named_setting_read_each_run() {
    let descriptor = crate::core::nodes::node_instance_factory::the_node_type_as_registered(
        the_node_type_of_a_project_view(&a_view_read_from("acme-dj:deck", the_deck_manifest()))
            .expect("a node view that opens is a node type"),
    )
    .expect("an output takes the name of the setting whose value it publishes")
    .descriptor;
    let [_waveform, setting] = descriptor.inputs.as_slice() else {
        panic!("the deck takes its declared input and one input of its output's name");
    };

    assert_eq!(
        (setting.name.as_str(), setting.description.as_str()),
        ("crossfader", "Left to right.")
    );
    assert_eq!(
        setting.family.families_an_input_takes(),
        vec![PortFamily::Array]
    );
    assert_eq!(
        setting.input_mode,
        Some(NodeInputMode::SettingOnly {
            read: SettingReadTime::EachRun
        }),
        "the input takes no wire and is read each run"
    );
    assert!(!setting.input_mode.unwrap().takes_a_wire());
    assert_eq!(
        setting.lifetime,
        NodeValueLifetime::Reset,
        "what a panel writes is live data, which no Stop keeps"
    );
    assert!(
        descriptor.outputs[0].flows_as_a_value(),
        "the output keeps its last message for a reader that connects later"
    );
}

#[test]
fn a_view_that_does_not_open_or_is_placed_in_an_area_is_no_node_type_and_says_why() {
    let mut no_title = the_deck_manifest();
    no_title.as_object_mut().unwrap().remove("title");
    let refusal = the_node_type_of_a_project_view(&a_view_read_from("acme-dj:deck", no_title))
        .expect_err("a view that does not open is no type");
    assert!(
        refusal.contains("`acme-dj:deck` does not open") && refusal.contains("title"),
        "{refusal}"
    );

    let mut in_an_area = the_deck_manifest();
    in_an_area["placement"] = json!("area");
    in_an_area.as_object_mut().unwrap().remove("inputs");
    in_an_area.as_object_mut().unwrap().remove("outputs");
    let refusal = the_node_type_of_a_project_view(&a_view_read_from("acme-dj:meter", in_an_area))
        .expect_err("an area view has no node");
    assert!(refusal.contains("placed in an area"), "{refusal}");
}

#[test]
fn an_image_or_packets_output_has_no_input_of_its_name_and_is_no_value() {
    let mut manifest = the_deck_manifest();
    manifest["outputs"] = json!([
        { "name": "preview", "family": "image" },
        { "name": "crossfader", "family": "array" }
    ]);
    let descriptor = the_node_type_of_a_project_view(&a_view_read_from("acme-dj:deck", manifest))
        .expect("a node view that opens is a node type");

    assert_eq!(
        descriptor
            .inputs
            .iter()
            .map(|port| port.name.as_str())
            .collect::<Vec<_>>(),
        ["waveform", "crossfader"],
        "no panel writes an Image output, so it has no input of its name"
    );
    assert!(!descriptor.outputs[0].flows_as_a_value());
    assert!(descriptor.outputs[1].flows_as_a_value());
}

#[test]
fn a_panel_writes_array_channels_and_records_outputs_alone() {
    assert_eq!(
        PortFamily::ALL
            .into_iter()
            .filter(|family| a_panel_writes(*family))
            .collect::<Vec<_>>(),
        [PortFamily::Array, PortFamily::Channels, PortFamily::Records]
    );
}

/// A view node handed a value one of its outputs does not carry makes no message of any other
/// output first: its refusal is that output's, and no output's value or next block moves. The
/// live set refuses such a value before it reaches the node; this holds for any delivery that
/// does not.
///
/// Mental-revert: writing each output as its message is made reaches `beat`, the first, and is
/// refused for the node holding no output writer, before `fader` is ever looked at.
#[test]
fn a_view_node_handed_a_value_one_output_does_not_carry_makes_no_message_of_any() {
    let descriptor = the_node_type_of_a_project_view(&a_view_read_from(
        "acme-dj:board",
        json!({
            "contract": "1.0", "title": "Board", "entry": "index.html", "placement": "node",
            "outputs": [{ "name": "beat", "family": "channels" },
                        { "name": "cue", "family": "records" },
                        { "name": "fader", "family": "array" }]
        }),
    ))
    .expect("a node view that opens is a node type");
    let node = GraphNode::new(
        descriptor.node_class_import_path.clone(),
        None,
        "board",
        Some(json!({})),
        descriptor
            .inputs
            .iter()
            .map(crate::core::graph::PortInfo::from)
            .collect(),
        descriptor
            .outputs
            .iter()
            .map(crate::core::graph::PortInfo::from)
            .collect(),
    );
    let mut board = ValueSourceNode::for_the_node(&node, descriptor);
    let set_live = ["beat", "cue", "fader"].map(String::from);
    let mut input_values = serde_json::Map::new();
    input_values.insert(
        "beat".into(),
        json!({ "samples": [[0.0, 0.0, 0.0]], "sample_rate": 48000 }),
    );
    input_values.insert("cue".into(), json!({ "track": "drop" }));
    input_values.insert("fader".into(), json!([[1], [2, 3]]));

    let refusal = board
        .publish_each_input_value_on_its_output(&input_values, &set_live)
        .expect_err("a ragged list is no Array value");

    assert!(
        refusal
            .to_string()
            .contains("output `fader` of node `board` carries Array"),
        "{refusal}"
    );
    assert!(board.current_value_of_each_output.is_empty());
    assert!(board.next_start_sample_of_each_channels_output.is_empty());
}
