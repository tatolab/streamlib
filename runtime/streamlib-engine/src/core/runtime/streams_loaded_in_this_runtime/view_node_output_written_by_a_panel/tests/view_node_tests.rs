// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A view node's ports: the node catalog's entry, writes onto its outputs, and a panel
//! reading its inputs.

use super::*;

#[test]
#[serial]
fn the_node_catalog_reports_a_view_type_with_its_manifests_description_and_ports() {
    let stream = AStreamOfAProjectBringingViews::load();

    let catalog = stream
        .stream
        .node_types_this_stream_resolves()
        .node_catalog();
    let deck = catalog
        .iter()
        .find(|descriptor| descriptor.processor_class_import_path.as_str() == DECK_VIEW_TYPE)
        .expect("the catalog lists the project's view");
    assert_eq!(deck.description, "Cues a track at the drop.");
    assert_eq!(
        deck.inputs
            .iter()
            .map(|port| (port.name.as_str(), port.description.as_str()))
            .collect::<Vec<_>>(),
        [("cue_in", "Cues to show.")]
    );
    assert_eq!(
        deck.outputs
            .iter()
            .map(|port| (port.name.as_str(), port.family.families_an_input_takes()))
            .collect::<Vec<_>>(),
        [
            ("cue", vec![PortFamily::Records]),
            ("fader", vec![PortFamily::Array])
        ]
    );
    assert!(
        !catalog
            .iter()
            .any(|descriptor| descriptor.processor_class_import_path.as_str() == "acme-dj:broken"),
        "a view that does not open is no type"
    );
}

#[test]
#[serial]
fn a_value_written_onto_a_view_nodes_output_reaches_its_reader_and_the_graph_keeps_none_of_it() {
    let stream = AStreamOfAProjectBringingViews::load();
    let deck_id = stream
        .add_a_node(DECK_VIEW_TYPE, "deck")
        .expect("a node of the view's type is added");
    let deck_uuid = stream.run(&deck_id);
    let graph_before = stream.what_the_graph_holds_of(&deck_id);
    let (_viewer, mut reader) = stream.a_reader_of(&deck_uuid, "cue");

    stream
        .write(&deck_uuid, "cue", json!({ "track": "intro", "beat": 32 }))
        .expect("the view node takes the record");
    let message = the_next_message_read(&mut reader);
    assert_eq!(
        message.payload_descriptor.known_kind().ok(),
        Some(DataChannelPayloadDescriptorKind::RecordsMessage)
    );
    let read_back: Value = rmp_serde::from_slice(&message.payload).unwrap();
    assert_eq!(read_back, json!({ "track": "intro", "beat": 32 }));

    stream
        .write(&deck_uuid, "cue", json!({ "track": "drop", "beat": 64 }))
        .expect("a later write is taken too");
    let message = the_next_message_read(&mut reader);
    let read_back: Value = rmp_serde::from_slice(&message.payload).unwrap();
    assert_eq!(read_back["track"], "drop", "the last write is what is read");

    assert_eq!(stream.what_the_graph_holds_of(&deck_id), graph_before);
}

#[test]
#[serial]
fn a_write_the_port_or_node_does_not_take_is_refused_naming_why() {
    let stream = AStreamOfAProjectBringingViews::load();
    let deck_id = stream.add_a_node(DECK_VIEW_TYPE, "deck").unwrap();
    let deck_uuid = stream.run(&deck_id);
    let camera_id = stream
        .add_a_node(
            MockOutputOnlyProcessor::processor_class_import_path().as_str(),
            "camera",
        )
        .unwrap();
    let camera_uuid = stream.run(&camera_id);

    let refusal = stream
        .write(&deck_uuid, "fader", json!({ "values": "loud" }))
        .unwrap_err()
        .to_string();
    assert!(
        refusal.contains("`fader`") && refusal.contains("carries Array"),
        "{refusal}"
    );
    let refusal = stream
        .write(&deck_uuid, "volume", json!(1))
        .unwrap_err()
        .to_string();
    assert!(refusal.contains("no output `volume`"), "{refusal}");
    let refusal = stream
        .write(&camera_uuid, "out1", json!({}))
        .unwrap_err()
        .to_string();
    assert!(refusal.contains("no view its project brings"), "{refusal}");
}

#[test]
#[serial]
fn a_write_onto_a_view_node_of_a_stopped_project_says_the_project_is_stopped_and_how_to_play_it() {
    let stream = AStreamOfAProjectBringingViews::load();
    let deck_id = stream.add_a_node(DECK_VIEW_TYPE, "deck").unwrap();
    let deck_uuid = stream.run(&deck_id);
    stream
        .stream
        .stop_playback_keeping_the_graph()
        .expect("the stream's playback stops");

    let refusal = stream
        .write(&deck_uuid, "fader", json!(0.5))
        .expect_err("a stopped project runs no node to publish")
        .to_string();
    assert!(refusal.contains("is stopped"), "{refusal}");
    assert!(refusal.contains("`start_playback` plays it"), "{refusal}");
    assert!(!refusal.contains("Resource not found"), "{refusal}");
}

#[test]
#[serial]
fn a_view_nodes_config_its_views_settings_do_not_declare_or_type_is_refused_naming_each_key() {
    let stream = AStreamOfAProjectBringingViews::load();
    let level_id = stream.add_a_node(LEVEL_VIEW_TYPE, "level").unwrap();

    stream
        .stream
        .update_processor_config(&level_id, json!({ "gain": 0.5 }))
        .expect("a setting the view declares, of its type, is taken");
    let refusal = stream
        .stream
        .update_processor_config(&level_id, json!({ "gain": "loud", "colour": 3 }))
        .expect_err("a setting of the wrong type and one the view does not declare are refused")
        .to_string();
    assert!(refusal.contains("`gain`"), "{refusal}");
    assert!(refusal.contains("\"loud\""), "{refusal}");
    assert!(refusal.contains("colour"), "{refusal}");
    assert!(refusal.contains(LEVEL_VIEW_TYPE), "{refusal}");

    let refusal = stream
        .stream
        .add_processor(
            ProcessorSpec::new(
                ProcessorClassImportPath::new(DECK_VIEW_TYPE).unwrap(),
                json!({ "bpm": 120 }),
            )
            .with_display_name("deck".to_string()),
        )
        .expect_err("a view that declares no settings takes none")
        .to_string();
    assert!(refusal.contains("bpm"), "{refusal}");
}

#[test]
#[serial]
fn a_second_node_of_one_view_in_a_stream_is_refused_naming_the_node_holding_it() {
    let stream = AStreamOfAProjectBringingViews::load();
    stream.add_a_node(DECK_VIEW_TYPE, "deck").unwrap();

    let refusal = stream
        .add_a_node(DECK_VIEW_TYPE, "deck-2")
        .expect_err("a stream holds one node of each view")
        .to_string();
    assert!(
        refusal.contains("node `deck`") && refusal.contains(DECK_VIEW_TYPE),
        "{refusal}"
    );
}

#[test]
#[serial]
fn a_node_naming_a_view_that_does_not_open_is_refused_with_why() {
    let stream = AStreamOfAProjectBringingViews::load();

    let refusal = stream
        .add_a_node("acme-dj:broken", "broken")
        .expect_err("a view that does not open is no type")
        .to_string();
    assert!(
        refusal.contains("`acme-dj:broken` does not open") && refusal.contains("title"),
        "{refusal}"
    );
}

#[test]
#[serial]
fn a_panel_reads_a_view_nodes_input_through_a_viewer_of_the_port_linked_into_it() {
    let stream = AStreamOfAProjectBringingViews::load();
    let camera_id = stream
        .add_a_node(
            MockOutputOnlyProcessor::processor_class_import_path().as_str(),
            "camera",
        )
        .unwrap();
    let camera_uuid = stream.run(&camera_id);
    let deck_id = stream.add_a_node(DECK_VIEW_TYPE, "deck").unwrap();
    stream.run(&deck_id);
    stream
        .stream
        .connect(
            OutputLinkPortRef::new(camera_id.clone(), "out1"),
            InputLinkPortRef::new(deck_id, "cue_in"),
        )
        .expect("the camera's output links into the view node's input");
    let (_viewer, mut reader) = stream.a_reader_of(&camera_uuid, "out1");

    let record = rmp_serde::to_vec_named(&json!({ "track": "outro" })).unwrap();
    stream
        .the_output_writer_of(&camera_id)
        .write_bytes_described_by(
            "out1",
            &record,
            10,
            MessageGroupBoundary::StartsGroup,
            streamlib_ipc_types::DataChannelPayloadDescriptor::records_message(),
        )
        .expect("the camera writes");
    let message = the_next_message_read(&mut reader);
    assert_eq!(message.payload, record);
}
