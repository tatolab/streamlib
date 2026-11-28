// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Each edit of a playing stream is applied at once or refused whole: its graph document equals
//! its running graph after every kind of edit, and an edit its running graph refuses leaves the
//! documents and the running graph as they were.

use super::*;
use streamlib_project_repository::SET_VARIABLE_NODE_TYPE;

const GAIN_NODE_UUID: &str = "5e5e5e5e-5e5e-4e5e-8e5e-5e5e5e5e5e5e";
const SETTER_INSIDE_NODE_UUID: &str = "4d4d4d4d-4d4d-4d4d-8d4d-4d4d4d4d4d4d";

impl AnEngineRecordingItsStreamsInATemporaryStateDirectory {
    /// Assert the graph document of `project`'s stream `main`, read with what it places, equals
    /// the running graph of the loaded stream `stream_id`, after the edit `edit`.
    fn assert_mains_document_equals_its_running_graph_after(
        &self,
        project: &ProjectWithGraphDocuments,
        stream_id: &str,
        edit: &str,
    ) {
        self.assert_the_document_equals_the_running_graph_after(project, "main", stream_id, edit);
    }

    /// Assert the graph document of `project`'s stream `stream_name`, read with what it places,
    /// equals the running graph of its loaded stream `stream_id`, after the edit `edit`.
    fn assert_the_document_equals_the_running_graph_after(
        &self,
        project: &ProjectWithGraphDocuments,
        stream_name: &str,
        stream_id: &str,
        edit: &str,
    ) {
        let running = self.the_live_graph_of(stream_id);
        let document = project.the_graph_document_as_its_project_reads_it(stream_name);
        assert_eq!(
            the_nodes_and_links_held_in(&document),
            the_nodes_and_links_held_in(&running),
            "after {edit}, {stream_name}'s document equals its running graph"
        );
    }

    /// `alice`'s transaction of `operations`, answered; panics unless it applies.
    async fn alices_transaction(&self, operations: Value) -> Value {
        self.the_answer_or_the_refusal_of(
            "transact",
            json!({ "author": an_agent_named("alice"), "operations": operations }),
        )
        .await
        .unwrap_or_else(|refused| panic!("the transaction applies: {refused:?}"))
    }

    /// `alice`'s undo or redo, `tool_name`, of her latest transaction on `project`; panics
    /// unless it applies.
    async fn alices_reversal(&self, project: &ProjectWithGraphDocuments, tool_name: &str) {
        self.the_answer_or_the_refusal_of(
            tool_name,
            json!({ "project_directory": project.path(), "author": an_agent_named("alice") }),
        )
        .await
        .unwrap_or_else(|refused| panic!("the {tool_name} applies: {refused:?}"));
    }
}

/// Every kind of edit, each an undo and a redo too, made on a playing stream leaves its graph
/// document equal to its running graph.
///
/// Mental-revert: an edit kind staged on the document and not reconciled into the running
/// graph, such as an unlock whose later remove is left to the documents alone, leaves the
/// running graph holding a node the document no longer does.
#[tokio::test(flavor = "multi_thread")]
async fn every_kind_of_edit_leaves_a_playing_streams_document_equal_to_its_running_graph() {
    input_value_test_node::register_the_input_value_test_node_once();
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_whose_main_stream_holds_one_source();
    let stream_id = engine.a_stream_started_from_its_graph_document(&project, "main");
    let main = project.stream_asset_of("main");
    let gain_type = input_value_test_node::InputValueTestNode::node_class_import_path();
    let sink_type = StreamToolsTestSink::node_class_import_path();
    let source = json!({ "node": SOURCE_NODE_UUID, "port": "video" });

    let added = engine
        .alices_transaction(json!([
            { "operation": "add_node", "stream": main, "as": "sink", "name": "monitor",
              "type": sink_type.as_str() },
            { "operation": "add_node", "stream": main, "as": "gain", "name": "gain",
              "type": gain_type.as_str() },
        ]))
        .await;
    engine.assert_mains_document_equals_its_running_graph_after(&project, &stream_id, "add_node");
    let sink = added["added_nodes"][0]["node"].as_str().unwrap().to_owned();
    let gain = added["added_nodes"][1]["node"].as_str().unwrap().to_owned();
    let into_the_sink = json!({ "node": sink, "port": "video" });

    for (edit, operations) in [
        (
            "connect",
            json!([{ "operation": "connect", "stream": main, "source": source,
                     "target": into_the_sink }]),
        ),
        (
            "disconnect",
            json!([{ "operation": "disconnect", "stream": main, "source": source,
                     "target": into_the_sink }]),
        ),
        (
            "connect again",
            json!([{ "operation": "connect", "stream": main, "source": source,
                     "target": into_the_sink }]),
        ),
        (
            "set_node_input_value",
            json!([{ "operation": "set_node_input_value", "stream": main, "node": gain,
                     "input": "gain", "value": 2.0 }]),
        ),
        (
            "rename_node",
            json!([{ "operation": "rename_node", "stream": main, "node": gain,
                     "name": "louder" }]),
        ),
        (
            "set_studio_fields",
            json!([{ "operation": "set_studio_fields", "stream": main, "node": gain,
                     "studio": { "position": [10.0, 20.0] } }]),
        ),
        (
            "add_group",
            json!([{ "operation": "add_group", "stream": main, "title": "loud",
                     "nodes": [gain] }]),
        ),
        (
            "add_note",
            json!([{ "operation": "add_note", "stream": main, "text": "turn it up",
                     "position": [0.0, 0.0], "size": [100.0, 40.0] }]),
        ),
    ] {
        engine.alices_transaction(operations).await;
        engine.assert_mains_document_equals_its_running_graph_after(&project, &stream_id, edit);
    }

    let collapsed = engine
        .alices_transaction(json!([
            { "operation": "collapse_selection_into_subgraph", "stream": main,
              "nodes": [sink], "name": "sub", "as": "sub" },
            { "operation": "rename_node", "stream": main, "node": "sub", "name": "sub-renamed" },
        ]))
        .await;
    engine.assert_mains_document_equals_its_running_graph_after(
        &project,
        &stream_id,
        "collapse_selection_into_subgraph, then an edit after it",
    );
    let subgraph = collapsed["added_nodes"][0]["node"]
        .as_str()
        .unwrap()
        .to_owned();

    engine
        .alices_transaction(json!([
            { "operation": "rename_node", "stream": main, "level": [subgraph],
              "node": the_uuid_a_held_node_takes_under(&subgraph, &sink), "name": "watcher" },
        ]))
        .await;
    engine.assert_mains_document_equals_its_running_graph_after(
        &project,
        &stream_id,
        "rename_node inside the stream it places",
    );
    let running = engine.the_live_graph_of(&stream_id);
    let mut placed_as_it_runs =
        the_node_names_a_fragment_places(&the_node_named(&running, "sub-renamed"));
    placed_as_it_runs.sort();
    assert_eq!(
        placed_as_it_runs,
        sorted_node_names_in(&project.the_graph_document_of("sub")),
        "the placement runs what sub's document holds: {running}"
    );
    assert_eq!(
        sorted_node_names_in(&project.the_graph_document_of("sub")),
        ["monitor-video", "watcher"]
    );

    engine
        .alices_transaction(json!([
            { "operation": "unlock_subgraph", "stream": main, "fragment": subgraph },
            { "operation": "remove_node", "stream": main, "node": gain },
        ]))
        .await;
    engine.assert_mains_document_equals_its_running_graph_after(
        &project,
        &stream_id,
        "unlock_subgraph, then a remove_node after it",
    );
    let unlocked = project.the_graph_document_of("main");
    assert_eq!(
        sorted_node_names_in(&unlocked),
        ["source", "watcher"],
        "{unlocked}"
    );

    for (reversal, edit) in [
        ("undo", "the undo of an unlock"),
        ("undo", "the undo of a rename inside a placed stream"),
        ("undo", "the undo of a collapse"),
        ("redo", "the redo of a collapse"),
        ("redo", "the redo of a rename inside a placed stream"),
        ("redo", "the redo of an unlock"),
    ] {
        engine.alices_reversal(&project, reversal).await;
        engine.assert_mains_document_equals_its_running_graph_after(&project, &stream_id, edit);
    }
}

/// A stream setting the variable `x` and placing `sub`, which sets its own `x` on its own level:
/// an unlock of the placement, which leaves two setters of `x` on the stream's level that its
/// running graph refuses, is refused whole: the stream's document, the placed stream's and the
/// running graph are as they were.
///
/// Mental-revert: making the unlock live only once the documents are written commits the
/// unlocked nodes to `main`'s document, answers the transaction as applied, and holds it not live
/// on the running stream.
#[tokio::test(flavor = "multi_thread")]
async fn an_unlock_the_running_graph_refuses_is_refused_whole() {
    input_value_test_node::register_the_input_value_test_node_once();
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_whose_main_stream_holds_one_source();
    let gain_type = input_value_test_node::InputValueTestNode::node_class_import_path();
    project.write_the_graph_document(json!({
        "stream": "sub",
        "nodes": [
            { "id": GAIN_NODE_UUID, "name": "gain", "type": gain_type.as_str() },
            { "id": SETTER_INSIDE_NODE_UUID, "name": "set_x_inside", "type": SET_VARIABLE_NODE_TYPE,
              "input_values": { "variable": "x", "node_reference": { "node": "gain" } } },
        ],
        "links": [],
    }));
    let placing_node = Uuid::new_v4().to_string();
    let mut main_placing_sub = project.the_graph_document_of("main");
    main_placing_sub["nodes"]
        .as_array_mut()
        .expect("main holds nodes")
        .extend([
            json!({ "id": Uuid::new_v4(), "name": "set_x", "type": SET_VARIABLE_NODE_TYPE,
                    "input_values": { "variable": "x", "node_reference": { "node": "source" } } }),
            json!({ "id": placing_node, "name": "sub", "type": STREAM_FRAGMENT_NODE_TYPE,
                    "input_values": { "stream": { "asset": project.stream_asset_of("sub") } } }),
        ]);
    project.write_the_graph_document(main_placing_sub);
    let stream_id = engine.a_stream_started_from_its_graph_document(&project, "main");
    let main = project.stream_asset_of("main");
    let main_before = project.the_graph_document_of("main");
    let sub_before = project.the_graph_document_of("sub");
    let running_before = the_nodes_and_links_held_in(&engine.the_live_graph_of(&stream_id));

    let refused = engine
        .the_answer_or_the_refusal_of(
            "transact",
            json!({ "author": an_agent_named("alice"), "operations": [
                { "operation": "unlock_subgraph", "stream": main, "fragment": placing_node },
            ] }),
        )
        .await
        .expect_err("main's running graph refuses two setters of `x` on its level");

    assert!(
        refused
            .runtime_words
            .contains("the variable `x` is set by both"),
        "main's own refusal reaches the caller: {refused:?}"
    );
    assert_eq!(project.the_graph_document_of("main"), main_before);
    assert_eq!(project.the_graph_document_of("sub"), sub_before);
    let running = engine.the_live_graph_of(&stream_id);
    assert_eq!(the_nodes_and_links_held_in(&running), running_before);
    assert_eq!(
        the_uuid_of_the_node_named(&running, "sub"),
        placing_node,
        "main still places sub"
    );
}

/// `main` and `zeta`, both playing, each place `sub` and link a source onto its port; a
/// transaction naming only `main` renames that port from inside `main`'s placement, which carries
/// `zeta`'s link onto the renamed port too: `zeta`'s running graph takes it in the same
/// transaction, and its document equals its running graph.
///
/// Mental-revert: bringing a stream the transaction does not name to its live graph as drawn,
/// not to its graph as the carry staged it, leaves `zeta` running its link onto the old port
/// name while its document holds the new one.
#[tokio::test(flavor = "multi_thread")]
async fn a_link_carried_onto_a_renamed_port_reaches_a_stream_the_transaction_does_not_name() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_whose_main_stream_holds_one_source();
    let main_id = engine.a_stream_started_from_its_graph_document(&project, "main");
    let main = project.stream_asset_of("main");
    let added = engine
        .alices_transaction(json!([
            { "operation": "add_node", "stream": main, "as": "sink", "name": "pass-sink",
              "type": StreamToolsTestSink::node_class_import_path().as_str() },
            { "operation": "connect", "stream": main,
              "source": { "node": SOURCE_NODE_UUID, "port": "video" },
              "target": { "node": "sink", "port": "video" } },
        ]))
        .await;
    let sink = added["added_nodes"][0]["node"].as_str().unwrap().to_owned();
    let collapsed = engine
        .alices_transaction(json!([
            { "operation": "collapse_selection_into_subgraph", "stream": main,
              "nodes": [sink], "name": "sub" },
        ]))
        .await;
    let subgraph = collapsed["added_nodes"][0]["node"]
        .as_str()
        .unwrap()
        .to_owned();
    let graph_input =
        the_uuid_of_the_node_named(&project.the_graph_document_of("sub"), "pass-sink-video");
    let zeta_source = Uuid::new_v4().to_string();
    let zeta_placement = Uuid::new_v4().to_string();
    project.write_the_graph_document(json!({
        "stream": "zeta",
        "nodes": [
            { "id": zeta_source, "name": "source",
              "type": StreamToolsTestSource::node_class_import_path().as_str() },
            { "id": zeta_placement, "name": "sub", "type": STREAM_FRAGMENT_NODE_TYPE,
              "input_values": { "stream": { "asset": project.the_graph_document_of("sub")["asset"] } } },
        ],
        "links": [
            { "source": { "node": zeta_source, "port": "video" },
              "target": { "node": zeta_placement, "port": graph_input } },
        ],
    }));
    let zeta_id = engine.a_stream_started_from_its_graph_document(&project, "zeta");
    engine.assert_the_document_equals_the_running_graph_after(
        &project,
        "zeta",
        &zeta_id,
        "zeta's load",
    );

    engine
        .alices_transaction(json!([
            { "operation": "set_node_input_value", "stream": main, "level": [subgraph],
              "node": the_uuid_a_held_node_takes_under(&subgraph, &graph_input),
              "input": "port", "value": "video_in" },
        ]))
        .await;

    assert_eq!(
        links_by_node_name_in(&engine.the_live_graph_of(&zeta_id)),
        ["source/video -> sub/video_in"],
        "zeta runs its link onto the renamed port"
    );
    engine.assert_the_document_equals_the_running_graph_after(
        &project,
        "zeta",
        &zeta_id,
        "a port of a stream it places renamed from another stream",
    );
    engine.assert_mains_document_equals_its_running_graph_after(
        &project,
        &main_id,
        "a port of a stream it places renamed from inside it",
    );
}
