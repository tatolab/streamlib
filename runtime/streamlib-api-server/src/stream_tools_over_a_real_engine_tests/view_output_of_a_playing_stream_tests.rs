// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A view node's output written by a panel on a playing stream, over a real engine:
//! `write_studio_view_output` sets the view node's input of the output's name live, the node
//! publishes each write on the output, a viewer opened later reads the last, and a value no panel
//! writes there, or a saved value on that input, is refused naming why.

use std::time::Instant;

use streamlib_ipc_types::{MessageReadFromAReservedSlot, OutputPortViewerReader};
use streamlib_runtime_client_contract::local_api_wire_contract::{
    ControlOperationRefusalCode, ControlOperationRefusalOnTheWire,
};

use super::input_value_set_tests::the_input_named;
use super::*;

/// How long a write on a playing stream may take to reach a viewer of the output.
const ARRIVES_WITHIN: Duration = Duration::from_secs(10);

/// The UUID the graph document gives its `deck`.
const DECK_NODE_UUID: &str = "d0ecd0ec-d0ec-4d0e-8d0e-d0ecd0ecd0ec";

/// A project whose stream `main` holds a node of its view `deck`, with a String output `cue`
/// and a Tensor output `fader`.
fn a_project_whose_stream_holds_the_deck() -> ProjectWithGraphDocuments {
    a_project_bringing_the_deck_whose_stream_holds(json!([
        { "id": DECK_NODE_UUID, "name": "deck", "type": format!("{TEST_PROJECT_NAME}:deck") },
    ]))
}

/// A project bringing the view `deck`, whose stream `main` holds `nodes`.
fn a_project_bringing_the_deck_whose_stream_holds(nodes: Value) -> ProjectWithGraphDocuments {
    let project = ProjectWithGraphDocuments::holding(json!({
        "stream": "main", "nodes": nodes, "links": [],
    }));
    let folder = project.path().join("views").join("deck");
    std::fs::create_dir_all(&folder).expect("the view's folder");
    std::fs::write(
        folder.join("tatolab_view.json"),
        json!({
            "contract": "1.1", "title": "Deck", "entry": "index.html", "placement": "node",
            "outputs": [{ "name": "cue", "pin_type": { "type": "string" } },
                        { "name": "fader", "pin_type": { "type": "tensor" } }],
        })
        .to_string(),
    )
    .expect("its manifest");
    project
}

impl AnEngineRecordingItsStreamsInATemporaryStateDirectory {
    /// The answer of `write_studio_view_output` writing `value` onto output `port` of the deck
    /// of `project`'s stream `main`, or its refusal.
    async fn write_onto_the_deck(
        &self,
        project: &ProjectWithGraphDocuments,
        port: &str,
        value: Value,
    ) -> std::result::Result<Value, Box<ControlOperationRefusalOnTheWire>> {
        self.the_answer_or_the_refusal_of(
            "write_studio_view_output",
            json!({ "stream": project.stream_id_of("main"), "node": DECK_NODE_UUID,
                    "port": port, "value": value }),
        )
        .await
    }

    /// A viewer of output `port` of the deck of `project`'s stream `main`, held open, and its
    /// reader.
    async fn a_viewer_of_the_deck(
        &self,
        project: &ProjectWithGraphDocuments,
        port: &str,
    ) -> (
        streamlib::sdk::runtime::OutputPortViewerHeldOpen,
        OutputPortViewerReader,
    ) {
        let viewer = self
            .engine
            .runtime_operations_of_the_stream_a_call_names(&project.stream_id_of("main"))
            .expect("the stream is loaded")
            .open_a_viewer_of_an_output_port(DECK_NODE_UUID.to_owned(), port.to_owned(), None)
            .await
            .expect("a running view node's output opens a viewer");
        let hand_off = viewer.hand_off().clone();
        let reader = OutputPortViewerReader::subscribe(
            &hand_off.iceoryx2_domain_root,
            &hand_off.channel_service_name,
            &hand_off.wake_service_name,
            "a-panel",
            hand_off.viewer_reads,
        )
        .expect("the viewer subscribes to the port's channel");
        (viewer, reader)
    }
}

/// The next message `reader` reads, failing when none arrives within the wait.
async fn the_next_message_read(
    reader: &mut OutputPortViewerReader,
) -> MessageReadFromAReservedSlot {
    let gave_up_at = Instant::now() + ARRIVES_WITHIN;
    loop {
        if let Some(message) = reader.read_the_latest_message().expect("the viewer reads") {
            return message;
        }
        assert!(Instant::now() < gave_up_at, "no message reached the viewer");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

fn the_record_of(message: &MessageReadFromAReservedSlot) -> Value {
    rmp_serde::from_slice(&message.payload).expect("a Records message decodes")
}

/// On a playing stream, a write onto a view node's output is the live value of its input of the
/// output's name, a viewer opened after it reads it, the same write again is published again,
/// and the graph document keeps none of it.
///
/// Mental-revert: a view node that publishes only a value that changed hands the viewer nothing
/// for the second write; an output that keeps no last message hands a viewer opened later
/// nothing at all.
#[tokio::test(flavor = "multi_thread")]
async fn a_write_onto_a_view_output_is_its_inputs_live_value_and_reaches_a_viewer() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_whose_stream_holds_the_deck();
    engine.playing(&project).await;
    let document_before = project.the_graph_document_of("main");
    let cue = json!("drop");

    let answer = engine
        .write_onto_the_deck(&project, "cue", cue.clone())
        .await
        .expect("a playing view node's output takes a string");
    assert_eq!(answer["port"], "cue", "{answer}");
    let (_viewer, mut reader) = engine.a_viewer_of_the_deck(&project, "cue").await;
    assert_eq!(
        the_record_of(&the_next_message_read(&mut reader).await),
        json!({ "value": cue }),
        "a viewer opened after the write reads the value the output keeps"
    );
    engine
        .write_onto_the_deck(&project, "cue", cue.clone())
        .await
        .expect("the same string is taken again");
    assert_eq!(
        the_record_of(&the_next_message_read(&mut reader).await),
        json!({ "value": cue }),
        "each write is published, the same as a new one"
    );

    let live_graph = engine.the_live_graph_of(&project.stream_id_of("main"));
    let cue_input = the_input_named(&the_node_named(&live_graph, "deck"), "cue");
    assert_eq!(cue_input["value_comes_from"], "live", "{cue_input}");
    assert_eq!(cue_input["value"], cue, "{cue_input}");
    assert_eq!(project.the_graph_document_of("main"), document_before);
    engine
        .the_project_action_done("stop_playback", &project)
        .await;
}

/// A value the output's pin type does not take is refused as a value its input does not take, at
/// `/value`; `null` is refused in the words of the output's family; and a value saved on the
/// input of an output, by a `commit`, is refused by name, since it holds no saved value.
#[tokio::test(flavor = "multi_thread")]
async fn a_write_no_panel_makes_or_a_saved_value_on_a_view_outputs_input_is_refused() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_whose_stream_holds_the_deck();
    engine.playing(&project).await;
    let document_before = project.the_graph_document_of("main");

    let refusal = engine
        .write_onto_the_deck(&project, "fader", json!({ "values": "loud" }))
        .await
        .expect_err("an Array output takes numbers");
    assert_eq!(
        refusal.refusal_code,
        ControlOperationRefusalCode::InputValueNotValid,
        "{refusal:?}"
    );
    assert_eq!(
        refusal.refused_params_pointer.as_deref(),
        Some("/value"),
        "{refusal:?}"
    );
    assert!(
        refusal.runtime_words.contains("`fader`")
            && refusal.runtime_words.contains("carries Array"),
        "{refusal:?}"
    );

    let refusal = engine
        .write_onto_the_deck(&project, "cue", Value::Null)
        .await
        .expect_err("null sets no value");
    assert!(
        refusal.runtime_words.contains(
            "output `cue` of node `deck` carries Records, and a Records value is a JSON object"
        ),
        "{refusal:?}"
    );

    let refusal = engine
        .the_answer_or_the_refusal_of(
            "set_node_input_value",
            json!({ "stream": project.stream_asset_of("main"), "node": DECK_NODE_UUID,
                    "input": "cue", "value": "intro", "mode": "commit" }),
        )
        .await
        .expect_err("the input of a view's output holds no saved value");
    assert_eq!(
        refusal.refusal_code,
        ControlOperationRefusalCode::InputTakesNoValue,
        "{refusal:?}"
    );
    assert!(
        refusal
            .runtime_words
            .contains("input `cue` of `deck` holds what a panel writes onto its output `cue`"),
        "{refusal:?}"
    );
    assert_eq!(project.the_graph_document_of("main"), document_before);
    engine
        .the_project_action_done("stop_playback", &project)
        .await;
}

/// An `add_node` of the deck saving a value on the input of its output `cue` is refused as
/// `input_takes_no_value`, at `/pins/cue`, and the graph document keeps nothing of it.
///
/// Mental-revert: an `add_node` whose pins go unchecked saves a value the input never holds.
#[tokio::test(flavor = "multi_thread")]
async fn an_add_node_saving_a_value_on_a_view_outputs_input_is_refused() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_bringing_the_deck_whose_stream_holds(json!([]));
    engine.playing(&project).await;
    let document_before = project.the_graph_document_of("main");

    let refusal = engine
        .the_answer_or_the_refusal_of(
            "add_node",
            json!({ "stream": project.stream_asset_of("main"),
                    "type": format!("{TEST_PROJECT_NAME}:deck"), "name": "deck",
                    "pins": { "cue": "intro" } }),
        )
        .await
        .expect_err("the input of a view's output holds no saved value");

    assert_eq!(
        refusal.refusal_code,
        ControlOperationRefusalCode::InputTakesNoValue,
        "{refusal:?}"
    );
    assert_eq!(
        refusal.refused_params_pointer.as_deref(),
        Some("/pins/cue"),
        "{refusal:?}"
    );
    assert_eq!(project.the_graph_document_of("main"), document_before);
    engine
        .the_project_action_done("stop_playback", &project)
        .await;
}

/// A graph document saving a value on the input of a view node's output loads, the value
/// dropped: the input renders no saved value.
#[tokio::test(flavor = "multi_thread")]
async fn a_document_saving_a_value_on_a_view_outputs_input_loads_with_the_value_dropped() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_bringing_the_deck_whose_stream_holds(json!([
        { "id": DECK_NODE_UUID, "name": "deck", "type": format!("{TEST_PROJECT_NAME}:deck"),
          "input_values": { "cue": "intro" } },
    ]));
    engine.playing(&project).await;

    let live_graph = engine.the_live_graph_of(&project.stream_id_of("main"));
    let deck = the_node_named(&live_graph, "deck");
    let cue_input = the_input_named(&deck, "cue");
    assert_ne!(cue_input["value_comes_from"], "saved", "{cue_input}");
    assert!(deck["pins"].get("cue").is_none(), "{deck}");
    engine
        .the_project_action_done("stop_playback", &project)
        .await;
}

/// A graph document saved with a value on the input of the deck's output, reloaded onto the
/// playing stream that holds the deck, reconciles with the value dropped: the deck is kept, and
/// its input renders no saved value.
///
/// Mental-revert: a reconcile that sets each pin the document holds on a kept node saves a value
/// the input never holds.
#[tokio::test(flavor = "multi_thread")]
async fn a_reload_saving_a_value_on_a_view_outputs_input_reconciles_with_the_value_dropped() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_whose_stream_holds_the_deck();
    engine.playing(&project).await;
    let deck_id_before = the_node_named(
        &engine.the_live_graph_of(&project.stream_id_of("main")),
        "deck",
    )["id"]
        .clone();

    let mut document = project.the_graph_document_of("main");
    document["nodes"][0]["input_values"]["cue"] = json!("intro");
    project.write_the_graph_document(document);
    engine
        .the_project_action_done("reload_project", &project)
        .await;

    let live_graph = engine.the_live_graph_of(&project.stream_id_of("main"));
    let deck = the_node_named(&live_graph, "deck");
    assert_eq!(deck["id"], deck_id_before, "the reload reconciles: {deck}");
    let cue_input = the_input_named(&deck, "cue");
    assert_ne!(cue_input["value_comes_from"], "saved", "{cue_input}");
    assert!(deck["pins"].get("cue").is_none(), "{deck}");
    engine
        .the_project_action_done("stop_playback", &project)
        .await;
}

/// A `live` transaction setting the deck's `cue` twice is refused at its second set before
/// anything is applied, naming the node and the input, whatever else it holds: two sets alone,
/// which the stream sets apart from the graph, a config set before them, which takes the
/// staged path, and two sets of a deck an `add_node` names by its `as`. Without the refusal the
/// second value would replace the first before the node is handed either, so the first would
/// never be published.
///
/// Mental-revert: checking the sets on the path that sets values apart from the graph alone lets
/// the second and third transactions through, the deck handed the second value alone.
#[tokio::test(flavor = "multi_thread")]
async fn a_live_transaction_setting_one_input_twice_is_refused_before_anything_is_applied() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_whose_stream_holds_the_deck();
    engine.playing(&project).await;
    let stream_asset = project.stream_asset_of("main");
    let set_cue_on = |node: &str, record: Value| {
        json!({ "operation": "set_node_input_value", "stream": stream_asset,
                "node": node, "input": "cue", "value": record })
    };
    let intro = json!("intro");
    let drop = json!("drop");
    let configuring_the_deck = json!({ "operation": "set_node_config", "stream": stream_asset,
                                       "node": DECK_NODE_UUID, "config": {} });
    let adding_a_second_deck = json!({ "operation": "add_node", "stream": stream_asset,
                                       "type": format!("{TEST_PROJECT_NAME}:deck"),
                                       "name": "second", "as": "second" });

    for (operations, second_index, node_named) in [
        (
            json!([
                set_cue_on(DECK_NODE_UUID, intro.clone()),
                set_cue_on(DECK_NODE_UUID, drop.clone())
            ]),
            1,
            "deck",
        ),
        (
            json!([
                configuring_the_deck,
                set_cue_on(DECK_NODE_UUID, intro.clone()),
                set_cue_on(DECK_NODE_UUID, drop.clone())
            ]),
            2,
            "deck",
        ),
        (
            json!([
                adding_a_second_deck,
                set_cue_on("second", intro.clone()),
                set_cue_on("second", drop.clone())
            ]),
            2,
            "second",
        ),
    ] {
        let refusal = engine
            .the_answer_or_the_refusal_of(
                "transact",
                json!({ "mode": "live", "author": an_agent_named("alice"),
                        "operations": operations }),
            )
            .await
            .expect_err("one live transaction sets each input once");

        assert_eq!(
            refusal.refusal_code,
            ControlOperationRefusalCode::Refused,
            "{operations}: {refusal:?}"
        );
        assert_eq!(
            refusal.refused_operation_index,
            Some(second_index),
            "{operations}: {refusal:?}"
        );
        assert!(
            refusal
                .runtime_words
                .contains(&format!("input `cue` of `{node_named}` is set twice")),
            "{operations}: {refusal:?}"
        );
    }
    let live_graph = engine.the_live_graph_of(&project.stream_id_of("main"));
    let deck = the_node_named(&live_graph, "deck");
    let cue_input = the_input_named(&deck, "cue");
    assert_ne!(cue_input["value_comes_from"], "live", "{cue_input}");
    assert!(
        live_graph["nodes"]
            .as_array()
            .is_some_and(|nodes| nodes.iter().all(|node| node["name"] != "second")),
        "{live_graph}"
    );
    engine
        .the_project_action_done("stop_playback", &project)
        .await;
}
