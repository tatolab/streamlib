// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! An import's stream placed as a fragment, followed through an update and an undo, and an edit
//! reaching a stream placed at any depth.

use super::*;

impl ProjectWithGraphDocuments {
    /// Add a [`StreamToolsTestSink`] named `name` to this project's stream `main` as one
    /// committed transaction, answering the commit that records it.
    fn commit_a_sink_named(&self, name: &str) -> String {
        register_the_stream_tools_test_sink_once();
        let mut project_repository =
            streamlib_project_repository::ProjectRepository::open(self.path())
                .expect("the project opens");
        project_repository
            .apply_project_operations(
                &streamlib_project_repository::ProjectTransactionAuthor {
                    kind: streamlib_project_repository::ProjectTransactionAuthorKind::Person,
                    name: "arm maintainer".to_owned(),
                },
                None,
                Vec::new(),
                vec![streamlib_project_repository::ProjectOperation::StreamGraph(
                    streamlib_project_repository::StreamGraphOperationOnAStream {
                        stream: self.stream_asset_of("main"),
                        graph_operation:
                            streamlib_project_repository::StreamGraphOperation::AddNode {
                                node: streamlib_project_repository::StreamGraphDocumentNode {
                                    id: Uuid::new_v4(),
                                    name: name.to_owned(),
                                    node_type: StreamToolsTestSink::node_class_import_path()
                                        .as_str()
                                        .to_owned(),
                                    input_values: serde_json::Map::new(),
                                    exposed_config_fields: Vec::new(),
                                    studio: Default::default(),
                                    fragment: None,
                                },
                                links: Vec::new(),
                            },
                    },
                )],
                streamlib_project_repository::ProjectTransactionLineage::NewEdit,
            )
            .expect("the sink is committed")
            .into_applied_transaction()
            .commit
    }
}

/// Project `cell` imports project `arm` and places `arm`'s stream as a fragment: the graph
/// document stores the reference alone, the live graph places the stream's nodes under UUIDv5
/// ids, an update over an edited folder waits for its confirmation, the confirmed update and
/// its undo reach the running stream, a fresh load places the import's stream from disk, and
/// the import is removed once no node references it.
#[tokio::test(flavor = "multi_thread")]
async fn an_imported_stream_placed_as_a_fragment_follows_its_folder_through_an_update_and_an_undo()
{
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let arm = a_project_whose_main_stream_holds_one_source();
    arm.commit_a_sink_named("shoulder");
    let shoulder = the_node_named(&arm.the_graph_document_of("main"), "shoulder")["id"]
        .as_str()
        .expect("the arm's shoulder holds a UUID")
        .to_owned();
    let cell = a_project_whose_main_stream_holds_one_source();
    let stream_id = engine.a_stream_loaded_from_its_graph_document(&cell, "main");
    let stream_asset = cell.stream_asset_of("main");
    let arm_stream = arm.stream_asset_of("main").to_string();
    let call = |tool_name: &'static str, arguments: Value| {
        let operations = engine.operations_on_the_loaded_streams();
        async move { tool_answer(&tool_call_result(operations, tool_name, arguments).await) }
    };

    let added = call(
        "add_dependency",
        json!({ "project_directory": cell.path(), "source": arm.path().display().to_string(),
                "author": an_agent_named("alice") }),
    )
    .await
    .expect("the arm is imported");
    let folder = added["dependency"]["folder"]
        .as_str()
        .expect("add_dependency answers the import's folder")
        .to_owned();
    let import_folder = cell.path().join("imports").join(&folder);
    assert!(
        import_folder.join("streams/main.graph.json").is_file(),
        "{added}"
    );
    let first_version = added["dependency"]["version"].clone();

    let used = call(
        "use_dependency_stream",
        json!({ "stream": stream_asset, "project": arm.project, "dependency_stream": arm_stream,
                "name": "arm", "author": an_agent_named("alice") }),
    )
    .await
    .expect("the arm's stream is placed");
    let fragment = used["added_nodes"][0]["node"]
        .as_str()
        .expect("use_dependency_stream answers the fragment node's UUID")
        .to_owned();
    let stored = the_node_named(&cell.the_graph_document_of("main"), "arm");
    assert_eq!(
        stored["input_values"],
        json!({ "stream": { "asset": arm_stream, "from": arm.project } }),
        "{stored}"
    );
    assert!(stored.get("fragment").is_none(), "{stored}");
    let live = the_node_named(&engine.the_live_graph_of(&stream_id), "arm");
    assert_eq!(
        the_node_names_a_fragment_places(&live),
        ["source", "shoulder"]
    );
    assert_eq!(
        the_node_named(&live["fragment"], "shoulder")["id"],
        json!(the_uuid_a_held_node_takes_under(&fragment, &shoulder)),
        "{live}"
    );
    let placed_again = call(
        "use_dependency_stream",
        json!({ "stream": stream_asset, "project": arm.project, "dependency_stream": arm_stream,
                "name": "arm", "author": an_agent_named("alice") }),
    )
    .await
    .expect_err("a name the graph already holds is not placed again");
    assert!(placed_again.contains("already places"), "{placed_again}");

    arm.commit_a_sink_named("elbow");
    let listed = call(
        "list_dependencies",
        json!({ "project_directory": cell.path(), "check_for_newer": true }),
    )
    .await
    .expect("the imports are listed");
    assert_eq!(
        listed["dependencies"][0]["dependency"]["version"], first_version,
        "{listed}"
    );
    assert_eq!(
        listed["dependencies"][0]["newer_version_exists"],
        json!(true),
        "{listed}"
    );
    let refused_while_used = call(
        "remove_dependency",
        json!({ "project_directory": cell.path(), "project": arm.project,
                "author": an_agent_named("alice") }),
    )
    .await
    .expect_err("an import a fragment references is not removed");
    assert!(
        refused_while_used.contains(&fragment),
        "{refused_while_used}"
    );

    std::fs::write(import_folder.join("notes.txt"), "edited in place\n")
        .expect("a file written into the import's folder");
    let waiting = call(
        "update_dependency",
        json!({ "project_directory": cell.path(), "project": arm.project,
                "author": an_agent_named("alice") }),
    )
    .await
    .expect("an update over edited files answers them");
    assert_eq!(
        waiting["overwrite_confirmation_needed"],
        json!(true),
        "{waiting}"
    );
    assert_eq!(waiting["edited_files"], json!(["notes.txt"]), "{waiting}");
    assert!(waiting.get("commit").is_none(), "{waiting}");
    assert_eq!(
        the_node_names_a_fragment_places(&the_node_named(
            &engine.the_live_graph_of(&stream_id),
            "arm"
        )),
        ["source", "shoulder"]
    );

    let updated = call(
        "update_dependency",
        json!({ "project_directory": cell.path(), "project": arm.project,
                "overwrite_edited_files": true, "author": an_agent_named("alice") }),
    )
    .await
    .expect("the import is copied again");
    assert_ne!(updated["dependency"]["version"], first_version, "{updated}");
    assert!(!import_folder.join("notes.txt").exists());
    assert_eq!(
        the_node_names_a_fragment_places(&the_node_named(
            &engine.the_live_graph_of(&stream_id),
            "arm"
        )),
        ["source", "shoulder", "elbow"]
    );
    assert!(
        the_node_named(&cell.the_graph_document_of("main"), "arm")
            .get("fragment")
            .is_none()
    );

    call(
        "undo",
        json!({ "project_directory": cell.path(), "author": an_agent_named("alice") }),
    )
    .await
    .expect("the update is undone");
    assert_eq!(
        the_node_names_a_fragment_places(&the_node_named(
            &engine.the_live_graph_of(&stream_id),
            "arm"
        )),
        ["source", "shoulder"]
    );
    let restarted = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let reloaded_stream_id = restarted.a_stream_loaded_from_its_graph_document(&cell, "main");
    let reloaded = the_node_named(&restarted.the_live_graph_of(&reloaded_stream_id), "arm");
    assert_eq!(
        the_node_names_a_fragment_places(&reloaded),
        ["source", "shoulder"]
    );

    call(
        "remove_node",
        json!({ "stream": stream_asset, "node": fragment, "author": an_agent_named("alice") }),
    )
    .await
    .expect("the fragment is taken out");
    let removed = call(
        "remove_dependency",
        json!({ "project_directory": cell.path(), "project": arm.project,
                "author": an_agent_named("alice") }),
    )
    .await
    .expect("an import no node references any longer is removed");
    assert_eq!(removed["dependency"]["folder"], json!(folder), "{removed}");
    assert!(!import_folder.exists());
    let listed_after = call(
        "list_dependencies",
        json!({ "project_directory": cell.path() }),
    )
    .await
    .expect("the imports are listed");
    assert_eq!(listed_after["dependencies"], json!([]), "{listed_after}");
}

/// The graph of the stream `stream_name` holding one fragment named after each stream of
/// `held_stream_names`, each a reference to `project`'s own stream.
fn the_graph_of_a_stream_holding_fragments_of(
    project: &ProjectWithGraphDocuments,
    stream_name: &str,
    held_stream_names: &[&str],
) -> Value {
    let fragment_nodes: Vec<Value> = held_stream_names
        .iter()
        .map(|held_stream_name| {
            json!({
                "id": Uuid::new_v4(),
                "name": held_stream_name,
                "type": STREAM_FRAGMENT_NODE_TYPE,
                "input_values": {
                    "stream": { "asset": project.stream_asset_of(held_stream_name) }
                },
            })
        })
        .collect();
    json!({ "stream": stream_name, "nodes": fragment_nodes, "links": [] })
}

/// `left` holds `main` and `top` holds `left`, all three loaded. An edit to `main` reaches the
/// fragment of it in `left` and, through `left`, in `top`, while their graph documents keep the
/// references alone; its undo reaches both again.
#[tokio::test(flavor = "multi_thread")]
async fn an_edit_to_a_stream_reaches_each_loaded_stream_holding_it_at_any_depth() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let cell = a_project_whose_main_stream_holds_one_source();
    cell.write_the_graph_document(the_graph_of_a_stream_holding_fragments_of(
        &cell,
        "left",
        &["main"],
    ));
    cell.write_the_graph_document(the_graph_of_a_stream_holding_fragments_of(
        &cell,
        "top",
        &["left"],
    ));
    engine.a_stream_loaded_from_its_graph_document(&cell, "main");
    let main_asset = cell.stream_asset_of("main");
    let left_id = engine.a_stream_loaded_from_its_graph_document(&cell, "left");
    let top_id = engine.a_stream_loaded_from_its_graph_document(&cell, "top");
    let main_in_top = |engine: &AnEngineRecordingItsStreamsInATemporaryStateDirectory| {
        let left_in_top = the_node_named(&engine.the_live_graph_of(&top_id), "left");
        the_node_names_a_fragment_places(&the_node_named(&left_in_top["fragment"], "main"))
    };
    assert_eq!(main_in_top(&engine), ["source"]);

    let sink_type = StreamToolsTestSink::node_class_import_path();
    tool_answer(
        &tool_call_result(
            engine.operations_on_the_loaded_streams(),
            "add_node",
            json!({ "stream": main_asset, "type": sink_type.as_str(), "name": "wrist",
                    "author": an_agent_named("alice") }),
        )
        .await,
    )
    .expect("main is edited");

    assert_eq!(
        the_node_names_a_fragment_places(&the_node_named(
            &engine.the_live_graph_of(&left_id),
            "main"
        )),
        ["source", "wrist"]
    );
    assert_eq!(main_in_top(&engine), ["source", "wrist"]);
    assert!(
        the_node_named(&cell.the_graph_document_of("left"), "main")
            .get("fragment")
            .is_none()
    );

    tool_answer(
        &tool_call_result(
            engine.operations_on_the_loaded_streams(),
            "undo",
            json!({ "project_directory": cell.path(), "author": an_agent_named("alice") }),
        )
        .await,
    )
    .expect("the edit is undone");
    assert_eq!(main_in_top(&engine), ["source"]);
}

/// `left` holds `main`; placing `left` in `main` would make each hold the other, so the edit is
/// refused naming both streams, and `main` is left as it was.
#[tokio::test(flavor = "multi_thread")]
async fn a_stream_fragment_that_would_close_a_cycle_is_refused_naming_each_stream() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let cell = a_project_whose_main_stream_holds_one_source();
    cell.write_the_graph_document(the_graph_of_a_stream_holding_fragments_of(
        &cell,
        "left",
        &["main"],
    ));
    let main_id = engine.a_stream_loaded_from_its_graph_document(&cell, "main");
    let main_asset = cell.stream_asset_of("main");

    let refusal = tool_answer(
        &tool_call_result(
            engine.operations_on_the_loaded_streams(),
            "transact",
            json!({ "operations": [
                { "operation": "add_node", "stream": main_asset, "type": STREAM_FRAGMENT_NODE_TYPE,
                  "name": "left", "as": "left" },
                { "operation": "set_node_input_value", "stream": main_asset, "node": "left",
                  "input": "stream", "value": { "asset": cell.stream_asset_of("left") } },
            ], "author": an_agent_named("alice") }),
        )
        .await,
    )
    .expect_err("a fragment closing a cycle is refused");

    assert!(
        refusal.contains("cycle") && refusal.contains("`main`") && refusal.contains("`left`"),
        "{refusal}"
    );
    assert_eq!(
        sorted_node_names_in(&engine.the_live_graph_of(&main_id)),
        ["source"]
    );
}

/// An open project runs `look` as its own stream until an edit places it in `main`, when it runs
/// only there; undoing the placement runs `look` as its own stream again.
#[tokio::test(flavor = "multi_thread")]
async fn a_stream_an_edit_places_runs_only_where_it_is_placed_until_the_edit_is_undone() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let operations_on_the_loaded_streams = &engine.operations_on_the_loaded_streams();
    let project = a_project_whose_main_stream_holds_one_source();
    let mut look = the_graph_of_a_stream_named("look");
    look["nodes"][0]["id"] = json!(Uuid::new_v4());
    project.write_the_graph_document(look);
    let look_id = project.stream_id_of("look");
    let main_id = project.stream_id_of("main");
    let opened =
        the_run_of_the_project_once_finished(operations_on_the_loaded_streams, project.path())
            .await;
    assert!(
        operations_on_the_loaded_streams
            .ids_of_the_loaded_streams()
            .contains(&look_id),
        "look runs as its own stream while nothing places it: {opened}"
    );

    tool_answer(
        &tool_call_result(
            Arc::clone(operations_on_the_loaded_streams),
            "transact",
            json!({ "operations": [
                { "operation": "add_node", "stream": project.stream_asset_of("main"),
                  "type": STREAM_FRAGMENT_NODE_TYPE, "name": "look", "as": "look" },
                { "operation": "set_node_input_value", "stream": project.stream_asset_of("main"),
                  "node": "look", "input": "stream",
                  "value": { "asset": project.stream_asset_of("look") } },
            ], "author": an_agent_named("alice") }),
        )
        .await,
    )
    .expect("main places look");
    let loaded_once_placed = operations_on_the_loaded_streams.ids_of_the_loaded_streams();
    assert!(
        !loaded_once_placed.contains(&look_id) && loaded_once_placed.contains(&main_id),
        "{loaded_once_placed:?}"
    );
    assert!(
        !engine
            .stream_records()
            .read_every()
            .into_iter()
            .flatten()
            .any(|record| record.stream_id() == look_id),
        "look is no stream of its own once main places it"
    );

    tool_answer(
        &tool_call_result(
            Arc::clone(operations_on_the_loaded_streams),
            "undo",
            json!({ "project_directory": project.path(), "author": an_agent_named("alice") }),
        )
        .await,
    )
    .expect("the undo applies");
    for action_under_way in operations_on_the_loaded_streams.stream_actions_under_way() {
        the_stream_action_once_finished(
            operations_on_the_loaded_streams,
            &action_under_way.action_id,
        )
        .await;
    }
    assert!(
        operations_on_the_loaded_streams
            .ids_of_the_loaded_streams()
            .contains(&look_id),
        "look runs as its own stream again once nothing places it"
    );
}
