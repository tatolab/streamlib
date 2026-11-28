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
        let running = self.the_live_graph_of(stream_id);
        assert!(
            !running.to_string().contains("\"live\":false"),
            "after {edit}, no part of the running graph waits to be made live: {running}"
        );
        let document = project.the_graph_document_as_its_project_reads_it("main");
        assert_eq!(
            the_nodes_and_links_held_in(&document),
            the_nodes_and_links_held_in(&running),
            "after {edit}, main's document equals its running graph"
        );
    }

    /// `alice`'s transaction of `operations` on `project`, answered.
    async fn alices_transaction(&self, operations: Value) -> Value {
        self.the_answer_or_the_refusal_of(
            "transact",
            json!({ "author": an_agent_named("alice"), "operations": operations }),
        )
        .await
        .unwrap_or_else(|refused| panic!("the transaction applies: {refused:?}"))
    }

    /// `alice`'s undo or redo, `tool_name`, of her latest transaction on `project`, answered.
    async fn alices_reversal(&self, project: &ProjectWithGraphDocuments, tool_name: &str) {
        self.the_answer_or_the_refusal_of(
            tool_name,
            json!({ "project_directory": project.path(), "author": an_agent_named("alice") }),
        )
        .await
        .unwrap_or_else(|refused| panic!("the {tool_name} applies: {refused:?}"));
    }
}

/// The UUID of the node `name` of `graph`.
fn the_uuid_of_the_node_named(graph: &Value, name: &str) -> String {
    the_entries_held_in(graph, "nodes")
        .find(|node| node["name"] == name)
        .and_then(|node| node["id"].as_str())
        .unwrap_or_else(|| panic!("the graph holds a node `{name}`: {graph}"))
        .to_owned()
}

/// Every kind of edit, each an undo and a redo too, made on a playing stream leaves its graph
/// document equal to its running graph, and no part of it waiting to be made live.
///
/// Mental-revert: making an edit that places a stream, or one following it, live only once the
/// documents hold it leaves the running graph behind the document whenever the stream refuses it.
#[tokio::test(flavor = "multi_thread")]
async fn every_kind_of_edit_leaves_a_playing_streams_document_equal_to_its_running_graph() {
    input_value_test_node::register_the_input_value_test_node_once();
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_whose_main_stream_holds_one_source();
    let stream_id = engine.a_stream_loaded_from_its_graph_document(&project, "main");
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
        ["monitor", "source"],
        "{unlocked}"
    );

    for (reversal, edit) in [
        ("undo", "the undo of an unlock"),
        ("undo", "the undo of a collapse"),
        ("redo", "the redo of a collapse"),
        ("redo", "the redo of an unlock"),
    ] {
        engine.alices_reversal(&project, reversal).await;
        engine.assert_mains_document_equals_its_running_graph_after(&project, &stream_id, edit);
    }
}

/// A stream setting the variable `x` and placing `sub`, which sets its own `x` on its own level:
/// an unlock of the placement, which leaves two setters of `x` on the stream's level that its
/// running graph refuses, is refused whole: the stream's document, the placed stream's and the
/// running graph are as they were, and no part of the running graph waits to be made live.
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
    let stream_id = engine.a_stream_loaded_from_its_graph_document(&project, "main");
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
    assert!(
        !running.to_string().contains("\"live\":false"),
        "no part of the running graph waits to be made live: {running}"
    );
    assert_eq!(
        the_uuid_of_the_node_named(&running, "sub"),
        placing_node,
        "main still places sub"
    );
}
