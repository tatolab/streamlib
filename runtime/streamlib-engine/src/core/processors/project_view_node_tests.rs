// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::path::PathBuf;

use serde_json::json;
use streamlib_ipc_types::{
    ArrayFrameAttributeResidency, ArrayFrameDescriptorBody, DataChannelPayloadDescriptorKind,
    array_frame_attribute_table_of,
};
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

    assert_eq!(
        descriptor.processor_class_import_path.as_str(),
        "acme-dj:deck"
    );
    assert_eq!(descriptor.processor_class_short_name.as_str(), "DJ Deck");
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
        [(
            "waveform".to_string(),
            "What plays.".to_string(),
            vec![PortFamily::Channels],
            false
        )]
    );
    assert_eq!(
        descriptor.inputs[0].delivery_profile.as_deref(),
        Some("newest"),
        "a link into a view node's input is wired as a channel, which takes a delivery profile"
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
fn a_record_written_onto_a_records_output_is_its_msgpack_map() {
    let (descriptor, payload) = the_message_a_value_written_onto_an_output_is(
        PortFamily::Records,
        &json!({ "cue": "drop", "beat": 33 }),
        0,
    )
    .unwrap();
    assert_eq!(
        descriptor.known_kind().ok(),
        Some(DataChannelPayloadDescriptorKind::RecordsMessage)
    );
    let read_back: JsonValue = rmp_serde::from_slice(&payload).unwrap();
    assert_eq!(read_back, json!({ "cue": "drop", "beat": 33 }));

    let refusal = the_message_a_value_written_onto_an_output_is(PortFamily::Records, &json!(3), 0)
        .unwrap_err();
    assert!(refusal.contains("JSON object"), "{refusal}");
}

#[test]
fn a_number_or_nested_list_written_onto_an_array_output_is_its_values_attribute() {
    let (descriptor, payload) = the_message_a_value_written_onto_an_output_is(
        PortFamily::Array,
        &json!([[0.5, -1.0], [0.25, 1.0], [0.0, 0.0]]),
        0,
    )
    .unwrap();
    let rows = array_frame_attribute_table_of(
        ArrayFrameDescriptorBody::from_descriptor_body(&descriptor.body),
        &payload,
    )
    .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].name, VIEW_NODE_ARRAY_OUTPUT_ATTRIBUTE_NAME);
    assert_eq!(rows[0].dtype, "float64");
    assert_eq!(rows[0].shape, vec![3, 2]);
    let ArrayFrameAttributeResidency::Cpu {
        payload_byte_offset,
        byte_len,
    } = rows[0].residency
    else {
        panic!("a CPU attribute");
    };
    let elements: Vec<f64> = payload
        [payload_byte_offset as usize..(payload_byte_offset + byte_len) as usize]
        .as_chunks::<8>()
        .0
        .iter()
        .map(|element| f64::from_le_bytes(*element))
        .collect();
    assert_eq!(elements, [0.5, -1.0, 0.25, 1.0, 0.0, 0.0]);

    let (descriptor, payload) =
        the_message_a_value_written_onto_an_output_is(PortFamily::Array, &json!(0.75), 0).unwrap();
    let rows = array_frame_attribute_table_of(
        ArrayFrameDescriptorBody::from_descriptor_body(&descriptor.body),
        &payload,
    )
    .unwrap();
    assert_eq!(rows[0].shape, vec![1]);

    let (descriptor, payload) = the_message_a_value_written_onto_an_output_is(
        PortFamily::Array,
        &json!({ "left": [1, 2], "right": 3 }),
        0,
    )
    .unwrap();
    let rows = array_frame_attribute_table_of(
        ArrayFrameDescriptorBody::from_descriptor_body(&descriptor.body),
        &payload,
    )
    .unwrap();
    assert_eq!(
        rows.iter()
            .map(|row| (row.name.as_str(), row.shape.clone()))
            .collect::<Vec<_>>(),
        [("left", vec![2]), ("right", vec![1])]
    );

    let refusal =
        the_message_a_value_written_onto_an_output_is(PortFamily::Array, &json!([[1, 2], [3]]), 0)
            .unwrap_err();
    assert!(refusal.contains("differ in length"), "{refusal}");
}

#[test]
fn a_number_written_onto_a_channels_output_is_one_control_value_and_a_block_runs_on() {
    let (descriptor, payload) =
        the_message_a_value_written_onto_an_output_is(PortFamily::Channels, &json!(0.5), 96)
            .unwrap();
    assert_eq!(
        ChannelsBlockDescriptorBody::from_descriptor_body(&descriptor.body),
        ChannelsBlockDescriptorBody {
            start_sample: 96,
            sample_rate: 0,
            channel_count: 1,
            samples_per_channel: 1,
            sample_type: ChannelsBlockSampleType::F32 as u32,
        }
    );
    assert_eq!(payload, 0.5f32.to_le_bytes());

    let (descriptor, payload) = the_message_a_value_written_onto_an_output_is(
        PortFamily::Channels,
        &json!({ "samples": [[0.0, 0.5], [1.0, -1.0]], "sample_rate": 48000 }),
        0,
    )
    .unwrap();
    let body = ChannelsBlockDescriptorBody::from_descriptor_body(&descriptor.body);
    assert_eq!(
        (
            body.sample_rate,
            body.channel_count,
            body.samples_per_channel
        ),
        (48_000, 2, 2)
    );
    assert_eq!(body.planar_payload_byte_len(), Some(payload.len()));

    let refusal = the_message_a_value_written_onto_an_output_is(
        PortFamily::Channels,
        &json!({ "samples": [[0.0], [1.0, 2.0]] }),
        0,
    )
    .unwrap_err();
    assert!(refusal.contains("same number of samples"), "{refusal}");
}

#[test]
fn an_image_or_packets_value_is_never_written_from_a_panel() {
    for family in [PortFamily::Image, PortFamily::Packets] {
        let refusal =
            the_message_a_value_written_onto_an_output_is(family, &json!({}), 0).unwrap_err();
        assert!(refusal.contains("never"), "{refusal}");
    }
}
