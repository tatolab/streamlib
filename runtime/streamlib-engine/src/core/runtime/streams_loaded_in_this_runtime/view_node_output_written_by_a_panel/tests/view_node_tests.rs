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
        .find(|descriptor| descriptor.node_class_import_path.as_str() == DECK_VIEW_TYPE)
        .expect("the catalog lists the project's view");
    assert_eq!(deck.description, "Cues a track at the drop.");
    assert_eq!(
        deck.inputs
            .iter()
            .map(|port| (port.name.as_str(), port.description.as_str()))
            .collect::<Vec<_>>(),
        [
            ("cue_in", "Cues to show."),
            ("cue", "The cue played."),
            ("fader", "")
        ],
        "each output a panel writes has an input of its name, whose value it publishes"
    );
    assert_eq!(
        deck.outputs
            .iter()
            .map(|port| (
                port.name.as_str(),
                port.pin_declaration
                    .as_ref()
                    .map(|pin| pin.pin_type.clone())
            ))
            .collect::<Vec<_>>(),
        [
            (
                "cue",
                Some(crate::core::descriptors::DeclaredPinType::Is {
                    pin_type: crate::core::descriptors::PinType::String {}
                })
            ),
            (
                "fader",
                Some(crate::core::descriptors::DeclaredPinType::Is {
                    pin_type: crate::core::descriptors::PinType::Tensor {}
                })
            )
        ]
    );
    assert!(
        !catalog
            .iter()
            .any(|descriptor| descriptor.node_class_import_path.as_str()
                == format!("{ACME_DJ}:broken")),
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
        .write(&deck_uuid, "cue", json!("intro"))
        .expect("the view node takes the string");
    let message = the_next_message_read(&mut reader);
    assert_eq!(
        message.payload_descriptor.known_kind().ok(),
        Some(DataChannelPayloadDescriptorKind::RecordsMessage)
    );
    let read_back: Value = rmp_serde::from_slice(&message.payload).unwrap();
    assert_eq!(read_back, json!({ "value": "intro" }));

    stream
        .write(&deck_uuid, "cue", json!("drop"))
        .expect("a later write is taken too");
    let message = the_next_message_read(&mut reader);
    let read_back: Value = rmp_serde::from_slice(&message.payload).unwrap();
    assert_eq!(read_back["value"], "drop", "the last write is what is read");

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
            MockOutputOnlyNode::node_class_import_path().as_str(),
            "camera",
        )
        .unwrap();
    let camera_uuid = stream.run(&camera_id);

    let refusal = stream
        .write(&deck_uuid, "fader", json!({ "values": "loud" }))
        .unwrap_err()
        .to_string();
    assert!(
        refusal.contains("`fader`") && refusal.contains("carries Tensor"),
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
fn a_view_nodes_setting_its_views_settings_do_not_declare_or_type_is_refused() {
    let stream = AStreamOfAProjectBringingViews::load();
    let level_id = stream.add_a_node(LEVEL_VIEW_TYPE, "level").unwrap();

    stream
        .stream
        .set_pin_value(&level_id, "gain", None, Some(json!(0.5)))
        .expect("a setting the view declares, of its type, is taken");
    let refusal = stream
        .stream
        .set_pin_value(&level_id, "gain", None, Some(json!("loud")))
        .expect_err("a setting of the wrong type is refused")
        .to_string();
    assert!(refusal.contains("`gain`"), "{refusal}");
    assert!(refusal.contains("\"loud\""), "{refusal}");
    assert!(refusal.contains(LEVEL_VIEW_TYPE), "{refusal}");
    let refusal = stream
        .stream
        .set_pin_value(&level_id, "colour", None, Some(json!(3)))
        .expect_err("a setting the view does not declare is refused")
        .to_string();
    assert!(refusal.contains("colour"), "{refusal}");

    let refusal = stream
        .stream
        .add_node(
            NodeSpec::new(
                NodeClassImportPath::new(DECK_VIEW_TYPE).unwrap(),
                json!({ "bpm": 120 }),
            )
            .with_display_name("deck".to_string()),
        )
        .expect_err("a view that declares no settings takes none")
        .to_string();
    assert!(refusal.contains("bpm"), "{refusal}");
}

/// Mental-revert: checking each setting on its own takes a node missing `gain`, which the
/// mixer's settings require.
#[test]
#[serial]
fn a_view_node_whose_settings_leave_out_one_they_require_is_refused() {
    let stream = AStreamOfAProjectBringingViews::load();
    let add_a_mixer_holding = |input_values: Value| {
        stream.stream.add_node(
            NodeSpec::new(
                NodeClassImportPath::new(MIXER_VIEW_TYPE).unwrap(),
                input_values,
            )
            .with_display_name("mixer".to_string()),
        )
    };

    let refusal = add_a_mixer_holding(json!({ "trim": 0.5 }))
        .expect_err("a mixer missing the gain its settings require is refused")
        .to_string();
    assert!(refusal.contains("gain"), "{refusal}");
    let mixer_id =
        add_a_mixer_holding(json!({ "gain": 1 })).expect("a mixer holding its gain is added");
    let refusal = stream
        .stream
        .set_pin_value(&mixer_id, "gain", None, None)
        .expect_err("taking off the gain its settings require is refused")
        .to_string();
    assert!(refusal.contains("gain"), "{refusal}");
}

/// Mental-revert: setting each value as it comes keeps `trim` set though `gain` is refused.
#[test]
#[serial]
fn several_values_set_on_a_node_one_of_which_is_refused_set_none() {
    let stream = AStreamOfAProjectBringingViews::load();
    let mixer_id = stream
        .stream
        .add_node(
            NodeSpec::new(
                NodeClassImportPath::new(MIXER_VIEW_TYPE).unwrap(),
                json!({ "gain": 1, "trim": 0 }),
            )
            .with_display_name("mixer".to_string()),
        )
        .expect("a mixer holding its gain is added");
    let a_set_of = |input: &str, value: Value| {
        crate::core::runtime::streams_loaded_in_this_runtime::loaded_stream::InputValueSetOnANode {
            input: input.to_owned(),
            member: None,
            value: Some(value),
        }
    };

    let refusal = stream
        .stream
        .set_the_input_values_of_a_node(
            &mixer_id,
            vec![
                a_set_of("trim", json!(0.5)),
                a_set_of("gain", json!("loud")),
            ],
        )
        .expect_err("a gain of the wrong type is refused")
        .to_string();
    assert!(refusal.contains("`gain`"), "{refusal}");
    let held = stream.stream.compiler.scope_reading(|graph| {
        graph
            .traversal()
            .v(&mixer_id)
            .first()
            .map(|mixer| mixer.input_values.clone())
            .expect("the mixer is in the stream")
    });
    assert_eq!(held, *json!({ "gain": 1, "trim": 0 }).as_object().unwrap());
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

/// A node of a view taken out and another of the same view added in one commit is taken: the
/// node leaving holds the view no longer.
///
/// Mental-revert: counting the node pending deletion refuses the new one as a second node of
/// the view.
#[test]
#[serial]
fn a_node_of_a_view_replaced_by_another_in_one_commit_is_taken() {
    let stream = AStreamOfAProjectBringingViews::load();
    let deck = stream.add_a_node(DECK_VIEW_TYPE, "deck").unwrap();

    stream
        .stream
        .in_one_commit_on_this_thread(|| {
            stream.stream.remove_node(&deck)?;
            stream.add_a_node(DECK_VIEW_TYPE, "deck-2")
        })
        .expect("the node leaving holds the view no longer");

    assert!(stream.stream.the_node_named("deck-2").is_ok());
    assert!(stream.stream.the_node_named("deck").is_err());
}

#[test]
#[serial]
fn a_node_naming_a_view_that_does_not_open_is_refused_with_why() {
    let stream = AStreamOfAProjectBringingViews::load();

    let refusal = stream
        .add_a_node(&format!("{ACME_DJ}:broken"), "broken")
        .expect_err("a view that does not open is no type")
        .to_string();
    assert!(
        refusal.contains(&format!("`{ACME_DJ}:broken` does not open")) && refusal.contains("title"),
        "{refusal}"
    );
}

#[test]
#[serial]
fn a_panel_reads_a_view_nodes_input_through_a_viewer_of_the_port_linked_into_it() {
    let stream = AStreamOfAProjectBringingViews::load();
    let camera_id = stream
        .add_a_node(
            MockOutputOnlyNode::node_class_import_path().as_str(),
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

    let record = rmp_serde::to_vec_named(&json!({ "value": "outro" })).unwrap();
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

#[test]
#[serial]
fn a_write_sets_the_view_nodes_input_of_the_outputs_name_which_takes_a_value_wire_only() {
    let stream = AStreamOfAProjectBringingViews::load();
    let camera_id = stream
        .add_a_node(
            MockOutputOnlyNode::node_class_import_path().as_str(),
            "camera",
        )
        .unwrap();
    let deck_id = stream.add_a_node(DECK_VIEW_TYPE, "deck").unwrap();
    let deck_uuid = stream.run(&deck_id);

    stream
        .write(&deck_uuid, "cue", json!("intro"))
        .expect("the view node takes the string");
    let live_value = stream.stream.compiler.scope(|graph, _tx| {
        graph
            .live_input_values()
            .value_of(deck_uuid.parse().unwrap(), "cue")
    });
    assert_eq!(
        live_value,
        Some(json!("intro")),
        "the value is the live value of the input `cue`"
    );

    let refusal = stream
        .stream
        .connect(
            OutputLinkPortRef::new(camera_id, "out1"),
            InputLinkPortRef::new(deck_id, "cue"),
        )
        .expect_err("the input of an output's name is a setting, which takes a Value wire only")
        .to_string();
    assert!(
        refusal.contains("a setting takes a `value` wire only"),
        "{refusal}"
    );
}

/// The Records message `message` carries, read back.
fn the_record_of(message: &MessageReadFromAReservedSlot) -> Value {
    rmp_serde::from_slice(&message.payload).unwrap()
}

/// The `start_sample` of the Channels block `message` carries.
fn the_start_sample_of(message: &MessageReadFromAReservedSlot) -> u64 {
    streamlib_ipc_types::ChannelsBlockDescriptorBody::from_descriptor_body(
        &message.payload_descriptor.body,
    )
    .start_sample
}

#[test]
#[serial]
fn the_same_string_written_twice_is_published_twice() {
    let stream = AStreamOfAProjectBringingViews::load();
    let deck_id = stream.add_a_node(DECK_VIEW_TYPE, "deck").unwrap();
    let deck_uuid = stream.run(&deck_id);
    let (_viewer, mut reader) = stream.a_reader_of(&deck_uuid, "cue");

    for _ in 0..2 {
        stream
            .write(&deck_uuid, "cue", json!("drop"))
            .expect("the view node takes the string");
        assert_eq!(
            the_record_of(&the_next_message_read(&mut reader)),
            json!({ "value": "drop" }),
            "each write is one message, the same event as much as a new one"
        );
    }
}

#[test]
#[serial]
fn the_same_channels_block_written_twice_is_two_blocks_one_after_the_other() {
    let stream = AStreamOfAProjectBringingViews::load();
    let pulse_id = stream.add_a_node(PULSE_VIEW_TYPE, "pulse").unwrap();
    let pulse_uuid = stream.run(&pulse_id);
    let (_viewer, mut reader) = stream.a_reader_of(&pulse_uuid, "beat");
    let silence = json!({ "samples": [[0.0, 0.0, 0.0]], "sample_rate": 48000 });

    stream
        .write(&pulse_uuid, "beat", silence.clone())
        .expect("the view node takes the block");
    let first = the_next_message_read(&mut reader);
    stream
        .write(&pulse_uuid, "beat", silence)
        .expect("the same block is taken again");
    let second = the_next_message_read(&mut reader);

    assert_eq!(the_start_sample_of(&first), 0);
    assert_eq!(
        the_start_sample_of(&second),
        3,
        "the second block starts where the first ends"
    );
}

/// A regression lock on the live set's own check of each value: a batch naming a value one
/// output does not carry is refused before the node is handed any, so nothing is published on
/// any output and each Channels output's next block stays where it was. The node's own making
/// of every message before it writes any is locked in `project_view_node`'s tests.
///
/// Mental-revert: a live set that hands each value to the node unchecked publishes `beat` and
/// `cue`, which come before `fader`, and runs the next block of `beat` on past one never
/// accepted.
#[test]
#[serial]
fn a_batch_one_output_does_not_carry_is_refused_before_the_node_publishes_any() {
    let stream = AStreamOfAProjectBringingViews::load();
    let board_id = stream.add_a_node(BOARD_VIEW_TYPE, "board").unwrap();
    let board_uuid = stream.run(&board_id);
    let (_beat_viewer, mut beat_reader) = stream.a_reader_of(&board_uuid, "beat");
    let (_cue_viewer, mut cue_reader) = stream.a_reader_of(&board_uuid, "cue");
    let (_fader_viewer, mut fader_reader) = stream.a_reader_of(&board_uuid, "fader");
    let silence = json!({ "samples": [[0.0, 0.0, 0.0]], "sample_rate": 48000 });
    stream.write(&board_uuid, "beat", silence.clone()).unwrap();
    assert_eq!(
        the_start_sample_of(&the_next_message_read(&mut beat_reader)),
        0
    );

    let refusal = stream
        .write_each(
            &board_uuid,
            &[
                ("beat", silence.clone()),
                ("cue", json!("drop")),
                ("fader", json!([[1], [2, 3]])),
            ],
        )
        .expect_err("a ragged list is no Array value");

    assert!(refusal.to_string().contains("`fader`"), "{refusal}");
    std::thread::sleep(Duration::from_millis(100));
    assert!(beat_reader.read_the_latest_message().unwrap().is_none());
    assert!(cue_reader.read_the_latest_message().unwrap().is_none());
    assert!(fader_reader.read_the_latest_message().unwrap().is_none());
    stream.write(&board_uuid, "beat", silence).unwrap();
    assert_eq!(
        the_start_sample_of(&the_next_message_read(&mut beat_reader)),
        3,
        "the refused batch ran no block on"
    );
}

#[test]
#[serial]
fn a_write_onto_one_output_publishes_nothing_on_another() {
    let stream = AStreamOfAProjectBringingViews::load();
    let deck_id = stream.add_a_node(DECK_VIEW_TYPE, "deck").unwrap();
    let deck_uuid = stream.run(&deck_id);
    let (_viewer, mut fader_reader) = stream.a_reader_of(&deck_uuid, "fader");
    stream.write(&deck_uuid, "fader", json!(0.5)).unwrap();
    the_next_message_read(&mut fader_reader);

    stream.write(&deck_uuid, "cue", json!("intro")).unwrap();
    stream.write(&deck_uuid, "cue", json!("intro")).unwrap();

    std::thread::sleep(Duration::from_millis(100));
    assert!(
        fader_reader.read_the_latest_message().unwrap().is_none(),
        "`fader` carries what it carried, and is not sent it again"
    );
}

#[test]
#[serial]
fn a_reader_that_connects_after_a_write_reads_the_value_its_output_keeps() {
    let stream = AStreamOfAProjectBringingViews::load();
    let deck_id = stream.add_a_node(DECK_VIEW_TYPE, "deck").unwrap();
    let deck_uuid = stream.run(&deck_id);
    let pulse_id = stream.add_a_node(PULSE_VIEW_TYPE, "pulse").unwrap();
    let pulse_uuid = stream.run(&pulse_id);

    stream.write(&deck_uuid, "cue", json!("outro")).unwrap();
    stream.write(&pulse_uuid, "tempo", json!(1)).unwrap();
    let (_cue_viewer, mut cue_reader) = stream.a_reader_of(&deck_uuid, "cue");
    let (_tempo_viewer, mut tempo_reader) = stream.a_reader_of(&pulse_uuid, "tempo");
    for node_id in [&deck_id, &pulse_id] {
        // What each pass of a running node's scheduling loop does; these nodes run none.
        stream
            .the_output_writer_of(node_id)
            .send_each_kept_message_to_a_reader_it_missed();
    }

    assert_eq!(
        the_record_of(&the_next_message_read(&mut cue_reader)),
        json!({ "value": "outro" })
    );
    let tempo = the_next_message_read(&mut tempo_reader);
    assert_eq!(
        tempo.payload,
        1.0_f64.to_le_bytes().to_vec(),
        "the control value written before the reader connected"
    );
}

#[test]
#[serial]
fn a_null_written_onto_an_output_is_refused_in_its_pin_types_words() {
    let stream = AStreamOfAProjectBringingViews::load();
    let deck_id = stream.add_a_node(DECK_VIEW_TYPE, "deck").unwrap();
    let deck_uuid = stream.run(&deck_id);

    let refusal = stream
        .write(&deck_uuid, "cue", Value::Null)
        .expect_err("null sets no value")
        .to_string();
    assert!(
        refusal.contains("output `cue` of node `deck` carries String")
            && refusal.contains("`null` sets no value"),
        "{refusal}"
    );
}
