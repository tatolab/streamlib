// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A selection collapsed into a stream placed where it was, and edits inside a stream placed once
//! or twice, each undone exactly.

use super::*;

impl AnEngineRecordingItsStreamsInATemporaryStateDirectory {
    /// What the tool `tool_name` answers `arguments` with on this engine, or its refusal.
    async fn the_answer_of_the_tool(
        &self,
        tool_name: &str,
        arguments: Value,
    ) -> std::result::Result<Value, String> {
        tool_answer(
            &tool_call_result(
                self.operations_on_the_loaded_streams(),
                tool_name,
                arguments,
            )
            .await,
        )
    }

    /// `author`'s undo of their latest transaction on `project`, or its refusal.
    async fn the_undo_by(
        &self,
        project: &ProjectWithGraphDocuments,
        author: &str,
    ) -> std::result::Result<Value, String> {
        self.the_answer_of_the_tool(
            "undo",
            json!({ "project_directory": project.path(), "author": an_agent_named(author) }),
        )
        .await
    }

    /// A [`StreamToolsTestSink`] `pass-sink` added to `project`'s `main` by `alice`, linked from
    /// its source: the sink's UUID.
    async fn a_sink_linked_from_the_source_of_main(
        &self,
        project: &ProjectWithGraphDocuments,
    ) -> String {
        let stream_asset = project.stream_asset_of("main");
        let added = self
            .the_answer_of_the_tool(
                "transact",
                json!({ "operations": [
                    { "operation": "add_node", "stream": stream_asset, "as": "sink",
                      "name": "pass-sink",
                      "type": StreamToolsTestSink::node_class_import_path().as_str() },
                    { "operation": "connect", "stream": stream_asset,
                      "source": { "node": SOURCE_NODE_UUID, "port": "video" },
                      "target": { "node": "sink", "port": "video" } },
                ], "author": an_agent_named("alice") }),
            )
            .await
            .expect("the sink is placed and linked");
        added["added_nodes"][0]["node"].as_str().unwrap().to_owned()
    }

    /// The node `node` of `project`'s `main` collapsed by `alice` into the subgraph `sub`: the
    /// subgraph node's UUID.
    async fn a_node_of_main_collapsed_into_sub(
        &self,
        project: &ProjectWithGraphDocuments,
        node: &str,
    ) -> String {
        let collapsed = self
            .the_answer_of_the_tool(
                "collapse_selection_into_subgraph",
                json!({ "stream": project.stream_asset_of("main"), "nodes": [node],
                        "name": "sub", "author": an_agent_named("alice") }),
            )
            .await
            .expect("the selection collapses");
        assert_eq!(collapsed["added_nodes"][0]["name"], "sub", "{collapsed}");
        collapsed["added_nodes"][0]["node"]
            .as_str()
            .unwrap()
            .to_owned()
    }
}

/// The stream asset the stream fragment node `placing_node` of `document` places.
fn the_stream_asset_placed_by(document: &Value, placing_node: &str) -> String {
    document["nodes"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|node| node["id"] == placing_node)
        .and_then(|node| node["input_values"]["stream"]["asset"].as_str())
        .unwrap_or_else(|| panic!("node {placing_node} places no stream: {document}"))
        .to_owned()
}

/// A sink collapsed into a subgraph is saved as the project's stream `sub` and runs where `sub`
/// is placed, fed through its port from the same source; a name a stream already holds is
/// refused; unlocking gives back a copy of the sink linked straight to the source; undoing the
/// unlock and the collapse gives back the exact earlier document and takes `sub` out, and a redo
/// saves it again.
#[tokio::test(flavor = "multi_thread")]
async fn a_collapsed_selection_is_saved_as_a_stream_running_where_it_is_placed_and_undoes_exactly()
{
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_whose_main_stream_holds_one_source();
    let stream_id = engine.a_stream_loaded_from_its_graph_document(&project, "main");
    let stream_asset = project.stream_asset_of("main");
    let sink = engine.a_sink_linked_from_the_source_of_main(&project).await;
    let before_the_collapse = project.the_graph_document_of("main");

    let subgraph = engine
        .a_node_of_main_collapsed_into_sub(&project, &sink)
        .await;

    let collapsed = project.the_graph_document_of("main");
    assert_eq!(sorted_node_names_in(&collapsed), ["source", "sub"]);
    let placing_node = the_node_named(&collapsed, "sub");
    assert_eq!(placing_node["type"], STREAM_FRAGMENT_NODE_TYPE);
    assert!(
        placing_node.get("fragment").is_none(),
        "main stores the reference alone: {placing_node}"
    );
    assert_eq!(
        links_by_node_name_in(&collapsed),
        ["source/video -> sub/pass-sink-video"]
    );
    let saved = project.the_graph_document_of("sub");
    assert_eq!(
        saved["asset"].as_str().unwrap(),
        the_stream_asset_placed_by(&collapsed, &subgraph)
    );
    assert_eq!(
        sorted_node_names_in(&saved),
        ["pass-sink", "pass-sink-video"]
    );
    assert_eq!(the_node_named(&saved, "pass-sink")["id"], json!(sink));
    assert_eq!(
        links_by_node_name_in(&saved),
        ["pass-sink-video/value -> pass-sink/video"]
    );
    let live = engine.the_live_graph_of(&stream_id);
    assert_eq!(
        links_by_node_name_in(&live),
        ["source/video -> sub/pass-sink-video"],
        "{live}"
    );
    let live_subgraph = the_node_named(&live, "sub");
    assert_eq!(
        the_node_names_a_fragment_places(&live_subgraph),
        ["pass-sink", "pass-sink-video"],
        "{live_subgraph}"
    );
    assert_eq!(
        the_node_named(&live_subgraph["fragment"], "pass-sink")["id"],
        json!(the_uuid_a_held_node_takes_under(&subgraph, &sink)),
        "{live_subgraph}"
    );
    assert!(
        live["links"][0]["id"]
            .as_str()
            .is_some_and(|link_id| !link_id.is_empty()),
        "the drawn link into the subgraph renders the running link it stands for: {live}"
    );

    let refused = engine
        .the_answer_of_the_tool(
            "collapse_selection_into_subgraph",
            json!({ "stream": stream_asset, "nodes": [SOURCE_NODE_UUID], "name": "main",
                    "author": an_agent_named("alice") }),
        )
        .await
        .expect_err("a stream's name is not taken twice");
    assert!(refused.contains("streams/main.graph.json"), "{refused}");

    engine
        .the_answer_of_the_tool(
            "unlock_subgraph",
            json!({ "stream": stream_asset, "fragment": subgraph,
                    "author": an_agent_named("alice") }),
        )
        .await
        .expect("the subgraph unlocks");
    let unlocked = project.the_graph_document_of("main");
    assert_eq!(sorted_node_names_in(&unlocked), ["pass-sink", "source"]);
    assert_ne!(
        the_node_named(&unlocked, "pass-sink")["id"],
        json!(sink),
        "the unlocked sink is a copy under a fresh UUID"
    );
    assert_eq!(
        links_by_node_name_in(&unlocked),
        ["source/video -> pass-sink/video"]
    );
    assert_eq!(
        links_by_node_name_in(&engine.the_live_graph_of(&stream_id)),
        ["source/video -> pass-sink/video"]
    );
    assert_eq!(
        project.the_graph_document_of("sub"),
        saved,
        "the unlocked stream stays a stream of the project"
    );

    for _ in 0..2 {
        engine
            .the_undo_by(&project, "alice")
            .await
            .expect("the undo applies");
    }
    assert_eq!(project.the_graph_document_of("main"), before_the_collapse);
    assert!(
        !project.path().join("streams/sub.graph.json").exists(),
        "the undo of the collapse takes the saved stream out"
    );
    let live_once_undone = engine.the_live_graph_of(&stream_id);
    assert_eq!(
        links_by_node_name_in(&live_once_undone),
        ["source/video -> pass-sink/video"]
    );
    assert_eq!(
        the_node_named(&live_once_undone, "pass-sink")["id"],
        json!(sink),
        "{live_once_undone}"
    );

    engine
        .the_answer_of_the_tool(
            "redo",
            json!({ "project_directory": project.path(), "author": an_agent_named("alice") }),
        )
        .await
        .expect("the redo applies");
    assert_eq!(project.the_graph_document_of("sub"), saved);
    assert_eq!(project.the_graph_document_of("main"), collapsed);
    assert_eq!(
        the_node_names_a_fragment_places(&the_node_named(
            &engine.the_live_graph_of(&stream_id),
            "sub"
        )),
        ["pass-sink", "pass-sink-video"]
    );
}

/// An undo of a collapse is refused while another stream of the project places the subgraph it
/// saved, the refusal naming that stream, and goes through once that placement is undone.
#[tokio::test(flavor = "multi_thread")]
async fn the_undo_of_a_collapse_is_refused_while_another_stream_places_its_subgraph() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_whose_main_stream_holds_one_source();
    let mut other = the_graph_of_a_stream_named("other");
    other["nodes"][0]["id"] = json!(Uuid::new_v4());
    project.write_the_graph_document(other);
    engine.a_stream_loaded_from_its_graph_document(&project, "main");
    engine.a_stream_loaded_from_its_graph_document(&project, "other");
    let sink = engine.a_sink_linked_from_the_source_of_main(&project).await;
    let before_the_collapse = project.the_graph_document_of("main");
    let subgraph = engine
        .a_node_of_main_collapsed_into_sub(&project, &sink)
        .await;
    let sub_asset = the_stream_asset_placed_by(&project.the_graph_document_of("main"), &subgraph);
    engine
        .the_answer_of_the_tool(
            "add_node",
            json!({ "stream": project.stream_asset_of("other"),
                    "type": STREAM_FRAGMENT_NODE_TYPE, "name": "sub",
                    "config": { "stream": { "asset": sub_asset } },
                    "author": an_agent_named("bob") }),
        )
        .await
        .expect("bob places sub in other");

    let refused = engine
        .the_undo_by(&project, "alice")
        .await
        .expect_err("alice's collapse is not undone while other places sub");

    assert!(refused.contains("streams/other.graph.json"), "{refused}");
    assert!(project.path().join("streams/sub.graph.json").exists());
    engine
        .the_undo_by(&project, "bob")
        .await
        .expect("bob's placement undoes");
    engine
        .the_undo_by(&project, "alice")
        .await
        .expect("alice's collapse undoes once nothing else places sub");
    assert_eq!(project.the_graph_document_of("main"), before_the_collapse);
    assert!(!project.path().join("streams/sub.graph.json").exists());
}

/// A node added, wired and renamed at the level of a stream `main` places twice is made on that
/// stream's own document and runs in both placements; renaming one of its ports from inside
/// carries the links onto it in both; each edit undoes to the exact earlier documents.
#[tokio::test(flavor = "multi_thread")]
async fn an_edit_inside_a_stream_placed_twice_reaches_both_placements_and_undoes_exactly() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_whose_main_stream_holds_one_source();
    let stream_id = engine.a_stream_loaded_from_its_graph_document(&project, "main");
    let stream_asset = project.stream_asset_of("main");
    let sink_type = StreamToolsTestSink::node_class_import_path()
        .as_str()
        .to_owned();
    let sink = engine.a_sink_linked_from_the_source_of_main(&project).await;
    let subgraph = engine
        .a_node_of_main_collapsed_into_sub(&project, &sink)
        .await;
    let sub_asset = the_stream_asset_placed_by(&project.the_graph_document_of("main"), &subgraph);
    let graph_input = the_node_named(&project.the_graph_document_of("sub"), "pass-sink-video");
    let graph_input_drawn =
        the_uuid_a_held_node_takes_under(&subgraph, graph_input["id"].as_str().unwrap());
    engine
        .the_answer_of_the_tool(
            "transact",
            json!({ "operations": [
                { "operation": "add_node", "stream": stream_asset, "as": "again",
                  "type": STREAM_FRAGMENT_NODE_TYPE, "name": "sub-again",
                  "config": { "stream": { "asset": sub_asset } } },
                { "operation": "connect", "stream": stream_asset,
                  "source": { "node": SOURCE_NODE_UUID, "port": "video" },
                  "target": { "node": "again", "port": "pass-sink-video" } },
            ], "author": an_agent_named("alice") }),
        )
        .await
        .expect("main places sub a second time");
    let main_before_the_edits = project.the_graph_document_of("main");
    let sub_before_the_edits = project.the_graph_document_of("sub");

    let added_inside = engine
        .the_answer_of_the_tool(
            "transact",
            json!({ "operations": [
                { "operation": "add_node", "stream": stream_asset, "level": [subgraph],
                  "as": "second", "type": sink_type },
                { "operation": "connect", "stream": stream_asset, "level": [subgraph],
                  "source": { "node": graph_input_drawn, "port": "value" },
                  "target": { "node": "second", "port": "video" } },
            ], "author": an_agent_named("alice") }),
        )
        .await
        .expect("a node is added and wired inside sub");
    let second_drawn = added_inside["added_nodes"][0]["node"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(
        added_inside["added_nodes"][0]["name"], "streamtoolstestsink",
        "{added_inside}"
    );
    engine
        .the_answer_of_the_tool(
            "rename_node",
            json!({ "stream": stream_asset, "level": [subgraph], "node": second_drawn,
                    "name": "second-sink", "author": an_agent_named("alice") }),
        )
        .await
        .expect("the node inside sub is renamed");

    assert_eq!(project.the_graph_document_of("main"), main_before_the_edits);
    let sub = project.the_graph_document_of("sub");
    assert_eq!(
        sorted_node_names_in(&sub),
        ["pass-sink", "pass-sink-video", "second-sink"]
    );
    let second_own = the_node_named(&sub, "second-sink")["id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(
        the_uuid_a_held_node_takes_under(&subgraph, &second_own),
        second_drawn,
        "the answer names the node as main draws it"
    );
    let live = engine.the_live_graph_of(&stream_id);
    for placement in ["sub", "sub-again"] {
        let live_placement = the_node_named(&live, placement);
        assert_eq!(
            the_node_names_a_fragment_places(&live_placement),
            ["pass-sink", "pass-sink-video", "second-sink"],
            "{live_placement}"
        );
        assert_eq!(
            links_by_node_name_in(&live_placement["fragment"]),
            [
                "pass-sink-video/value -> pass-sink/video",
                "pass-sink-video/value -> second-sink/video"
            ],
            "{live_placement}"
        );
    }

    let mut renamed_port_config = graph_input["input_values"].clone();
    renamed_port_config["port"] = json!("video_in");
    engine
        .the_answer_of_the_tool(
            "set_node_config",
            json!({ "stream": stream_asset, "level": [subgraph], "node": graph_input_drawn,
                    "config": renamed_port_config, "author": an_agent_named("alice") }),
        )
        .await
        .expect("sub's port is renamed from inside it");
    let links_onto_the_renamed_port = [
        "source/video -> sub/video_in",
        "source/video -> sub-again/video_in",
    ];
    assert_eq!(
        links_by_node_name_in(&project.the_graph_document_of("main")),
        links_onto_the_renamed_port,
        "the links onto the renamed port follow it in both placements"
    );
    assert_eq!(
        links_by_node_name_in(&engine.the_live_graph_of(&stream_id)),
        links_onto_the_renamed_port,
        "the running links onto the renamed port follow it in both placements"
    );

    for _ in 0..3 {
        engine
            .the_undo_by(&project, "alice")
            .await
            .expect("the undo applies");
    }
    assert_eq!(project.the_graph_document_of("main"), main_before_the_edits);
    assert_eq!(project.the_graph_document_of("sub"), sub_before_the_edits);
    let live_once_undone = engine.the_live_graph_of(&stream_id);
    for placement in ["sub", "sub-again"] {
        assert_eq!(
            the_node_names_a_fragment_places(&the_node_named(&live_once_undone, placement)),
            ["pass-sink", "pass-sink-video"]
        );
    }
}

/// A transaction that collapses a selection and then renames the subgraph by its label reaches
/// the running stream in that order, and so does its redo.
#[tokio::test(flavor = "multi_thread")]
async fn a_collapse_and_a_rename_of_its_subgraph_in_one_transaction_reach_the_running_stream_in_order()
 {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_whose_main_stream_holds_one_source();
    let stream_id = engine.a_stream_loaded_from_its_graph_document(&project, "main");
    let stream_asset = project.stream_asset_of("main");
    let sink = engine.a_sink_linked_from_the_source_of_main(&project).await;
    let running_look_places_the_sink = |live: &Value| {
        assert_eq!(sorted_node_names_in(live), ["look", "source"], "{live}");
        assert_eq!(
            the_node_names_a_fragment_places(&the_node_named(live, "look")),
            ["pass-sink", "pass-sink-video"]
        );
    };

    engine
        .the_answer_of_the_tool(
            "transact",
            json!({ "operations": [
                { "operation": "collapse_selection_into_subgraph", "stream": stream_asset,
                  "nodes": [sink], "name": "sub", "as": "subgraph" },
                { "operation": "rename_node", "stream": stream_asset, "node": "subgraph",
                  "name": "look" },
            ], "author": an_agent_named("alice") }),
        )
        .await
        .expect("the collapse and the rename apply in order");
    assert_eq!(
        sorted_node_names_in(&project.the_graph_document_of("main")),
        ["look", "source"]
    );
    running_look_places_the_sink(&engine.the_live_graph_of(&stream_id));

    engine
        .the_undo_by(&project, "alice")
        .await
        .expect("the transaction undoes");
    assert_eq!(
        sorted_node_names_in(&engine.the_live_graph_of(&stream_id)),
        ["pass-sink", "source"]
    );
    engine
        .the_answer_of_the_tool(
            "redo",
            json!({ "project_directory": project.path(), "author": an_agent_named("alice") }),
        )
        .await
        .expect("the redo applies in order");
    running_look_places_the_sink(&engine.the_live_graph_of(&stream_id));
}

/// An add inside a placed stream of a type the stream does not resolve is refused, as the same
/// add at the stream's own level is, and leaves the placed stream as it was.
#[tokio::test(flavor = "multi_thread")]
async fn an_add_inside_a_placed_stream_of_a_type_nothing_resolves_is_refused() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_whose_main_stream_holds_one_source();
    engine.a_stream_loaded_from_its_graph_document(&project, "main");
    let sink = engine.a_sink_linked_from_the_source_of_main(&project).await;
    let subgraph = engine
        .a_node_of_main_collapsed_into_sub(&project, &sink)
        .await;
    let sub_before_the_add = project.the_graph_document_of("sub");
    let unresolved_type = StreamToolsTestSink::node_class_import_path()
        .as_str()
        .replace("StreamToolsTestSink", "StreamToolsTestSinkNothingRegisters");

    let refused = engine
        .the_answer_of_the_tool(
            "add_node",
            json!({ "stream": project.stream_asset_of("main"), "level": [subgraph],
                    "type": unresolved_type, "name": "missing",
                    "author": an_agent_named("alice") }),
        )
        .await
        .expect_err("a type nothing resolves is not added inside sub");

    assert!(
        refused.contains("StreamToolsTestSinkNothingRegisters"),
        "{refused}"
    );
    assert_eq!(project.the_graph_document_of("sub"), sub_before_the_add);
    engine
        .the_answer_of_the_tool(
            "add_node",
            json!({ "stream": project.stream_asset_of("main"), "level": [subgraph],
                    "type": GRAPH_OUTPUT_NODE_TYPE, "name": "out",
                    "config": { "port": "out", "family": "image" },
                    "author": an_agent_named("alice") }),
        )
        .await
        .expect("a Graph Output is added inside sub");
    assert_eq!(
        sorted_node_names_in(&project.the_graph_document_of("sub")),
        ["out", "pass-sink", "pass-sink-video"]
    );
}

/// A transaction that collapses one placement of `sub` into a new stream and then renames a
/// port of `sub` from inside its other placement carries the link onto that port in the new
/// stream too.
#[tokio::test(flavor = "multi_thread")]
async fn a_port_renamed_inside_a_placed_stream_is_carried_in_a_stream_the_same_transaction_saves() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_whose_main_stream_holds_one_source();
    engine.a_stream_loaded_from_its_graph_document(&project, "main");
    let stream_asset = project.stream_asset_of("main");
    let sink = engine.a_sink_linked_from_the_source_of_main(&project).await;
    let subgraph = engine
        .a_node_of_main_collapsed_into_sub(&project, &sink)
        .await;
    let sub_asset = the_stream_asset_placed_by(&project.the_graph_document_of("main"), &subgraph);
    let placed_again = engine
        .the_answer_of_the_tool(
            "transact",
            json!({ "operations": [
                { "operation": "add_node", "stream": stream_asset, "as": "again",
                  "type": STREAM_FRAGMENT_NODE_TYPE, "name": "sub-again",
                  "config": { "stream": { "asset": sub_asset } } },
                { "operation": "connect", "stream": stream_asset,
                  "source": { "node": SOURCE_NODE_UUID, "port": "video" },
                  "target": { "node": "again", "port": "pass-sink-video" } },
            ], "author": an_agent_named("alice") }),
        )
        .await
        .expect("main places sub a second time");
    let sub_again = placed_again["added_nodes"][0]["node"]
        .as_str()
        .unwrap()
        .to_owned();
    let graph_input = the_node_named(&project.the_graph_document_of("sub"), "pass-sink-video");
    let mut renamed_port_config = graph_input["input_values"].clone();
    renamed_port_config["port"] = json!("video_in");

    engine
        .the_answer_of_the_tool(
            "transact",
            json!({ "operations": [
                { "operation": "collapse_selection_into_subgraph", "stream": stream_asset,
                  "nodes": [subgraph], "name": "wrapper" },
                { "operation": "set_node_config", "stream": stream_asset, "level": [sub_again],
                  "node": the_uuid_a_held_node_takes_under(
                      &sub_again, graph_input["id"].as_str().unwrap()),
                  "config": renamed_port_config },
            ], "author": an_agent_named("alice") }),
        )
        .await
        .expect("the collapse and the rename apply");

    let links_into_sub_in_wrapper: Vec<String> =
        links_by_node_name_in(&project.the_graph_document_of("wrapper"))
            .into_iter()
            .filter(|link| link.contains("-> sub/"))
            .collect();
    assert_eq!(
        links_into_sub_in_wrapper.len(),
        1,
        "{links_into_sub_in_wrapper:?}"
    );
    assert!(
        links_into_sub_in_wrapper[0].ends_with("-> sub/video_in"),
        "{links_into_sub_in_wrapper:?}"
    );
    assert_eq!(
        links_by_node_name_in(&project.the_graph_document_of("main"))
            .into_iter()
            .filter(|link| link.contains("sub-again"))
            .collect::<Vec<_>>(),
        ["source/video -> sub-again/video_in"]
    );
}

/// Unlocking the placement of an import's stream copies its nodes into the project with each
/// stream they place still read from the import.
#[tokio::test(flavor = "multi_thread")]
async fn an_unlocked_placement_of_an_imports_stream_keeps_each_stream_it_places_in_the_import() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let arm = ProjectWithGraphDocuments::holding(json!({
        "stream": "main",
        "nodes": [{ "id": Uuid::new_v4(), "name": "grain", "type": STREAM_FRAGMENT_NODE_TYPE }],
        "links": [],
    }));
    let grain_asset = arm.stream_asset_of("grain");
    let mut arm_main = arm.the_graph_document_of("main");
    arm_main["nodes"][0]["input_values"] = json!({ "stream": { "asset": grain_asset } });
    arm.write_the_graph_document_text("main", &arm_main.to_string());
    let mut grain = the_graph_of_a_stream_named("grain");
    grain["nodes"][0]["id"] = json!(Uuid::new_v4());
    arm.write_the_graph_document(grain);
    let cell = a_project_whose_main_stream_holds_one_source();
    let stream_id = engine.a_stream_loaded_from_its_graph_document(&cell, "main");
    let stream_asset = cell.stream_asset_of("main");
    engine
        .the_answer_of_the_tool(
            "add_dependency",
            json!({ "project_directory": cell.path(), "source": arm.path().display().to_string(),
                    "author": an_agent_named("alice") }),
        )
        .await
        .expect("the arm is imported");
    let used = engine
        .the_answer_of_the_tool(
            "use_dependency_stream",
            json!({ "stream": stream_asset, "project": arm.project,
                    "dependency_stream": arm.stream_asset_of("main").to_string(), "name": "arm",
                    "author": an_agent_named("alice") }),
        )
        .await
        .expect("the arm's stream is placed");
    let fragment = used["added_nodes"][0]["node"].as_str().unwrap().to_owned();

    engine
        .the_answer_of_the_tool(
            "unlock_subgraph",
            json!({ "stream": stream_asset, "fragment": fragment,
                    "author": an_agent_named("alice") }),
        )
        .await
        .expect("the arm's placement unlocks");

    let unlocked = cell.the_graph_document_of("main");
    assert_eq!(sorted_node_names_in(&unlocked), ["grain", "source"]);
    assert_eq!(
        the_node_named(&unlocked, "grain")["input_values"],
        json!({ "stream": { "asset": grain_asset, "from": arm.project } })
    );
    assert_eq!(
        the_node_names_a_fragment_places(&the_node_named(
            &engine.the_live_graph_of(&stream_id),
            "grain"
        )),
        ["source"]
    );
}

/// A collapse naming a node the stream does not hold is refused naming that node, the node the
/// refusal names, and not the subgraph the collapse would make.
#[tokio::test(flavor = "multi_thread")]
async fn a_collapse_naming_a_node_the_stream_does_not_hold_is_refused_naming_that_node() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_whose_main_stream_holds_one_source();
    engine.a_stream_loaded_from_its_graph_document(&project, "main");
    let saved_before = project.the_graph_document_of("main");

    let refusal = engine
        .the_answer_or_the_refusal_of(
            "collapse_selection_into_subgraph",
            json!({ "stream": project.stream_asset_of("main"),
                    "nodes": [SOURCE_NODE_UUID, "absent"], "name": "sub",
                    "author": an_agent_named("alice") }),
        )
        .await
        .expect_err("a collapse naming a node the stream does not hold is refused");

    assert_eq!(
        refusal.refusal_code,
        streamlib_runtime_client_contract::local_api_wire_contract::ControlOperationRefusalCode::NodeNotFound,
        "{refusal:?}"
    );
    assert_eq!(
        refusal.refused_node.as_deref(),
        Some("absent"),
        "{refusal:?}"
    );
    assert_eq!(project.the_graph_document_of("main"), saved_before);
}
