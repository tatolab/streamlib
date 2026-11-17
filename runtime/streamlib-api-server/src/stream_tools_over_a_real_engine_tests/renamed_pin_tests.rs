// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A renamed pin still takes what was saved, recorded, kept and sent under the name it had, over
//! a real engine: a saved value and link land on it, an edit recorded under the old name undoes
//! onto it, a kept live value follows it, a request naming it so is answered under its name, and
//! the next commit saves only current names. An input that may be unset starts its node holding
//! no value and no wire.

use streamlib_project_repository::{
    ProjectOperation, ProjectRepository, ProjectStore, ProjectStoreTableName,
    ProjectTransactionAuthor, ProjectTransactionLineage, StreamGraphOperation,
    StreamGraphOperationOnAStream,
};

use super::input_value_set_tests::{the_input_named, the_input_values_of};
use super::*;

/// A native node whose input `gain` was named `level`, whose input `trim` may be unset, and whose
/// output `signal` was named `out`.
#[streamlib::sdk::node(
    execution = manual,
    input("gain", delivery_profile = "newest", pin_type = Float, former_names = ["level"]),
    input("trim", delivery_profile = "newest", pin_type = Float, may_be_unset = true),
    output("signal", pin_type = Float, former_names = ["out"]),
)]
pub(super) struct RenamedPinTestNode;

impl ManualNode for RenamedPinTestNode::Node {
    fn start(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        Ok(())
    }
}

fn register_the_renamed_pin_test_node_once() {
    static REGISTERED: std::sync::Once = std::sync::Once::new();
    REGISTERED.call_once(|| NODE_REGISTRY.register::<RenamedPinTestNode::Node>());
}

const DIMMER_NODE_UUID: &str = "8d8d8d8d-8d8d-4d8d-8d8d-8d8d8d8d8d8d";
const FADER_NODE_UUID: &str = "9e9e9e9e-9e9e-4e9e-8e9e-9e9e9e9e9e9e";

/// The table of a project's store the live values of `kept` inputs are kept in, spelled out to
/// pin the on-disk format from outside.
const LIVE_INPUT_VALUES_TABLE: ProjectStoreTableName =
    ProjectStoreTableName::named("live_input_values");

/// A project whose stream `main` was saved before the rename: its [`RenamedPinTestNode`]
/// `dimmer` holds `level` at 0.3, and a link runs from the dimmer's `out` into the `level` of
/// its `fader`.
fn a_project_saved_under_the_names_the_pins_had() -> ProjectWithGraphDocuments {
    register_the_renamed_pin_test_node_once();
    let node_type = RenamedPinTestNode::node_class_import_path();
    ProjectWithGraphDocuments::holding(json!({
        "stream": "main",
        "nodes": [
            { "id": DIMMER_NODE_UUID, "name": "dimmer", "type": node_type.as_str(),
              "config": {}, "pins": { "level": 0.3 } },
            { "id": FADER_NODE_UUID, "name": "fader", "type": node_type.as_str(),
              "config": {} },
        ],
        "links": [
            { "source": { "node": DIMMER_NODE_UUID, "port": "out" },
              "target": { "node": FADER_NODE_UUID, "port": "level" } },
        ],
    }))
}

impl AnEngineRecordingItsStreamsInATemporaryStateDirectory {
    /// What input `input` of node `node` of the stream `stream_id` renders: its value and where
    /// it comes from.
    fn what_the_input_reads(&self, stream_id: &str, node: &str, input: &str) -> (Value, Value) {
        let rendered = the_input_named(
            &the_node_named(&self.the_live_graph_of(stream_id), node),
            input,
        );
        (
            rendered["value"].clone(),
            rendered["value_comes_from"].clone(),
        )
    }

    /// Run the project action `tool_name` on `project` and wait until it is done.
    async fn the_action_on_the_project_done(
        &self,
        tool_name: &str,
        project: &ProjectWithGraphDocuments,
    ) {
        let finished = the_project_action_once_finished(
            tool_name,
            &self.operations_on_the_loaded_streams(),
            project.path(),
        )
        .await;
        assert_eq!(finished["state"], "done", "{tool_name}: {finished}");
    }
}

/// Each link `graph_document` saves, as `source/port -> target/port` by node UUID.
fn the_links_saved_in(graph_document: &Value) -> Vec<String> {
    graph_document["links"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|link| {
            format!(
                "{}/{} -> {}/{}",
                link["source"]["node"].as_str().unwrap_or_default(),
                link["source"]["port"].as_str().unwrap_or_default(),
                link["target"]["node"].as_str().unwrap_or_default(),
                link["target"]["port"].as_str().unwrap_or_default(),
            )
        })
        .collect()
}

/// A stream saved under the names its pins had loads with the value and the link on the renamed
/// pins. A commit naming the old input is made and answered under its name, and saves the
/// stream with only current names, the link moved too; undoing it puts the saved document back
/// as it was, and the old names still land.
///
/// Mental-revert: finding a port by exact name alone leaves the link set aside and `gain` saved
/// as nothing; answering the request as sent answers `level`; moving no old name in the commit
/// saves `level` beside `gain` and the link under its old ports.
#[tokio::test(flavor = "multi_thread")]
async fn a_stream_saved_under_old_pin_names_loads_onto_the_renamed_pins_and_saves_their_names() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_saved_under_the_names_the_pins_had();
    let stream_id = engine.a_stream_loaded_from_its_graph_document(&project, "main");

    assert_eq!(
        engine.what_the_input_reads(&stream_id, "dimmer", "gain"),
        (json!(0.3), json!("saved"))
    );
    assert_eq!(
        links_by_node_name_in(&engine.the_live_graph_of(&stream_id)),
        ["dimmer/signal -> fader/gain"]
    );

    let applied = engine
        .the_answer_or_the_refusal_of(
            "set_node_input_value",
            json!({ "stream": project.stream_asset_of("main"), "node": DIMMER_NODE_UUID,
                    "input": "level", "value": 0.6, "author": an_agent_named("alice") }),
        )
        .await
        .expect("a value set under the old name is taken");
    assert_eq!(applied["operations"][0]["input"], "gain", "{applied}");
    assert_eq!(applied["operations"][0]["from"], 0.3, "{applied}");
    assert_eq!(
        engine.what_the_input_reads(&stream_id, "dimmer", "gain"),
        (json!(0.6), json!("saved"))
    );
    let saved = project.the_graph_document_of("main");
    assert_eq!(
        the_input_values_of(&saved, DIMMER_NODE_UUID),
        json!({ "gain": 0.6 })
    );
    assert_eq!(
        the_links_saved_in(&saved),
        [format!(
            "{DIMMER_NODE_UUID}/signal -> {FADER_NODE_UUID}/gain"
        )]
    );

    engine
        .the_answer_or_the_refusal_of(
            "undo",
            json!({ "project_directory": project.path(), "author": an_agent_named("alice") }),
        )
        .await
        .expect("the commit is undone");
    let saved = project.the_graph_document_of("main");
    assert_eq!(
        the_input_values_of(&saved, DIMMER_NODE_UUID),
        json!({ "level": 0.3 })
    );
    assert_eq!(
        the_links_saved_in(&saved),
        [format!("{DIMMER_NODE_UUID}/out -> {FADER_NODE_UUID}/level")]
    );
    assert_eq!(
        engine.what_the_input_reads(&stream_id, "dimmer", "gain"),
        (json!(0.3), json!("saved"))
    );
    assert_eq!(
        links_by_node_name_in(&engine.the_live_graph_of(&stream_id)),
        ["dimmer/signal -> fader/gain"]
    );
}

/// A link saved under the old port names is taken out by a request naming those names, which
/// is answered under the current ones, and the saved stream no longer holds it.
///
/// Mental-revert: leaving the request's names as sent refuses it, the running stream holding no
/// link from `out` into `level`.
#[tokio::test(flavor = "multi_thread")]
async fn a_request_naming_ports_by_their_old_names_is_answered_under_their_names() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_saved_under_the_names_the_pins_had();
    let stream_id = engine.a_stream_loaded_from_its_graph_document(&project, "main");

    let disconnected = engine
        .the_answer_or_the_refusal_of(
            "disconnect",
            json!({ "stream": project.stream_asset_of("main"),
                    "source": { "node": DIMMER_NODE_UUID, "port": "out" },
                    "target": { "node": FADER_NODE_UUID, "port": "level" },
                    "author": an_agent_named("alice") }),
        )
        .await
        .expect("the link is taken out by its old port names");
    let answered = &disconnected["operations"][0]["link"];
    assert_eq!(answered["source"]["port"], "signal", "{disconnected}");
    assert_eq!(answered["target"]["port"], "gain", "{disconnected}");
    assert!(links_by_node_name_in(&engine.the_live_graph_of(&stream_id)).is_empty());
    assert!(the_links_saved_in(&project.the_graph_document_of("main")).is_empty());
}

/// An edit recorded before the rename, naming the old input, is undone and redone onto the
/// renamed input of the running stream.
///
/// Mental-revert: applying the recorded name as it is refuses the undo, the running node having
/// no input `level`.
#[tokio::test(flavor = "multi_thread")]
async fn an_edit_recorded_under_the_old_name_undoes_and_redoes_onto_the_renamed_input() {
    let project = a_project_saved_under_the_names_the_pins_had();
    let alice: ProjectTransactionAuthor =
        serde_json::from_value(an_agent_named("alice")).expect("an author reads");
    ProjectRepository::open(project.path())
        .expect("the project opens")
        .apply_project_operations(
            &alice,
            None,
            Vec::new(),
            vec![ProjectOperation::StreamGraph(
                StreamGraphOperationOnAStream {
                    stream: project.stream_asset_of("main"),
                    graph_operation: StreamGraphOperation::SetNodeInputValue {
                        node: Uuid::parse_str(DIMMER_NODE_UUID).expect("a UUID"),
                        input: "level".to_owned(),
                        from: Some(json!(0.3)),
                        to: Some(json!(0.9)),
                    },
                },
            )],
            ProjectTransactionLineage::NewEdit,
        )
        .expect("the edit is recorded under the old name");

    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    the_run_of_the_project_once_finished(
        &engine.operations_on_the_loaded_streams(),
        project.path(),
    )
    .await;
    let stream_id = project.stream_id_of("main");
    assert_eq!(
        engine.what_the_input_reads(&stream_id, "dimmer", "gain"),
        (json!(0.9), json!("saved"))
    );

    for (action, gain) in [("undo", 0.3), ("redo", 0.9)] {
        engine
            .the_answer_or_the_refusal_of(
                action,
                json!({ "project_directory": project.path(), "author": an_agent_named("alice") }),
            )
            .await
            .unwrap_or_else(|refusal| panic!("the recorded edit takes {action}: {refusal:?}"));
        assert_eq!(
            engine.what_the_input_reads(&stream_id, "dimmer", "gain"),
            (json!(gain), json!("saved")),
            "after {action}"
        );
    }
    engine
        .the_action_on_the_project_done("close_project", &project)
        .await;
}

/// A live value kept under the old name of an input is the renamed input's live value once the
/// stream loads, and the project's store keeps it under the input's name from then on, unless
/// it keeps one under that name already, which wins.
///
/// Mental-revert: finding the kept input by exact name forgets the value; moving it in memory
/// alone leaves the store keeping it under `level`.
#[tokio::test(flavor = "multi_thread")]
async fn a_live_value_kept_under_the_old_name_follows_the_renamed_input() {
    let project = a_project_saved_under_the_names_the_pins_had();
    {
        let project_store =
            ProjectStore::open_in_the_project_at(project.path()).expect("the store opens");
        for (key, value) in [
            (format!("{DIMMER_NODE_UUID}/level"), json!(0.8)),
            (format!("{FADER_NODE_UUID}/level"), json!(0.2)),
            (format!("{FADER_NODE_UUID}/gain"), json!(0.4)),
        ] {
            project_store
                .write(LIVE_INPUT_VALUES_TABLE, &key, &value)
                .expect("the kept value is written");
        }
    }

    {
        let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
        the_run_of_the_project_once_finished(
            &engine.operations_on_the_loaded_streams(),
            project.path(),
        )
        .await;
        let stream_id = project.stream_id_of("main");
        assert_eq!(
            engine.what_the_input_reads(&stream_id, "dimmer", "gain"),
            (json!(0.8), json!("live"))
        );
        engine
            .the_action_on_the_project_done("close_project", &project)
            .await;
    }

    let mut kept = ProjectStore::open_in_the_project_at(project.path())
        .expect("the store opens again")
        .keys_in(LIVE_INPUT_VALUES_TABLE)
        .expect("the kept values list");
    kept.sort();
    assert_eq!(
        kept,
        [
            format!("{DIMMER_NODE_UUID}/gain"),
            format!("{FADER_NODE_UUID}/gain")
        ]
    );
    let fader_gain = ProjectStore::open_in_the_project_at(project.path())
        .expect("the store opens again")
        .read(LIVE_INPUT_VALUES_TABLE, &format!("{FADER_NODE_UUID}/gain"))
        .expect("the kept value reads");
    assert_eq!(fader_gain, Some(json!(0.4)));
}

/// An input that may be unset starts its node with no value and no wire, reading nothing.
///
/// Mental-revert: refusing a node an input of which holds no value keeps the playback from
/// starting.
#[tokio::test(flavor = "multi_thread")]
async fn an_input_that_may_be_unset_starts_its_node_with_no_value_and_no_wire() {
    let project = a_project_saved_under_the_names_the_pins_had();
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    the_run_of_the_project_once_finished(
        &engine.operations_on_the_loaded_streams(),
        project.path(),
    )
    .await;
    engine
        .the_action_on_the_project_done("start_playback", &project)
        .await;
    let stream_id = project.stream_id_of("main");
    let dimmer = the_node_named(&engine.the_live_graph_of(&stream_id), "dimmer");
    assert_eq!(dimmer["components"]["state"], "Running", "{dimmer}");
    assert!(
        dimmer.get("inputs_waiting_for_a_value").is_none(),
        "{dimmer}"
    );
    assert_eq!(
        engine.what_the_input_reads(&stream_id, "dimmer", "trim"),
        (Value::Null, Value::Null)
    );
    engine
        .the_action_on_the_project_done("close_project", &project)
        .await;
}
