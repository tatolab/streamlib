// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Every graph edit names its stream by the stream id `list_streams` answers or by its stream
//! asset's UUID: an edit sent by either records one action, under the stream asset's UUID, whose
//! undo works once the project is loaded again; and a stream id the runtime holds no stream
//! under is refused with `stream_not_found`, naming the streams it holds.

use super::*;

/// The answer of the graph edit `tool_name` sent `arguments` by alice, failing the test on a
/// refusal.
async fn the_edit_applied(
    engine: &AnEngineRecordingItsStreamsInATemporaryStateDirectory,
    tool_name: &str,
    mut arguments: Value,
) -> Value {
    arguments["author"] = an_agent_named("alice");
    engine
        .the_answer_or_the_refusal_of(tool_name, arguments)
        .await
        .unwrap_or_else(|refusal| panic!("{tool_name} is refused: {refusal:?}"))
}

/// The actions history records in its newest `count` transactions, oldest first.
fn the_actions_recorded_oldest_first(
    project: &ProjectWithGraphDocuments,
    count: usize,
) -> Vec<Value> {
    streamlib_project_repository::ProjectRepository::open(project.path())
        .expect("the project opens")
        .applied_transactions_newest_first(count)
        .expect("history reads")
        .into_iter()
        .rev()
        .map(|applied| serde_json::to_value(&applied.transaction.actions).unwrap())
        .collect()
}

/// Each graph edit sent the stream id `list_streams` answers applies to that stream: a node
/// added and linked, renamed, moved, set, grouped, noted, unlinked and taken out, each alone and
/// inside `transact`, and every edit recorded under the stream asset's UUID.
#[tokio::test(flavor = "multi_thread")]
async fn every_graph_edit_takes_the_stream_id_list_streams_answers() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_whose_main_stream_holds_one_source();
    let stream_id = engine.a_stream_loaded_from_its_graph_document(&project, "main");
    let stream_asset = project.stream_asset_of("main");
    let sink_type = StreamToolsTestSink::node_class_import_path();

    let added = the_edit_applied(
        &engine,
        "add_node",
        json!({ "stream": stream_id, "type": sink_type.as_str(), "name": "monitor" }),
    )
    .await;
    assert_eq!(added["added_nodes"][0]["stream"], stream_id, "{added}");
    let monitor = added["added_nodes"][0]["node"]
        .as_str()
        .expect("add_node answers the node's UUID")
        .to_owned();
    super::input_value_set_tests::register_the_input_value_test_node_once();
    let mixed = the_edit_applied(
        &engine,
        "add_node",
        json!({ "stream": stream_id, "name": "mixer",
                "type": super::input_value_test_node::InputValueTestNode::node_class_import_path().as_str() }),
    )
    .await;
    let mixer = mixed["added_nodes"][0]["node"].as_str().unwrap().to_owned();
    the_edit_applied(
        &engine,
        "set_node_config",
        json!({ "stream": stream_id, "node": mixer, "config": { "frame_width": 640 } }),
    )
    .await;
    the_edit_applied(
        &engine,
        "set_pin_value",
        json!({ "stream": stream_id, "node": mixer, "input": "gain", "value": 0.5 }),
    )
    .await;
    the_edit_applied(
        &engine,
        "set_node_input_value",
        json!({ "stream": stream_id, "node": mixer, "input": "gain", "value": 0.25 }),
    )
    .await;
    let refused_exposure = engine
        .the_answer_or_the_refusal_of(
            "set_exposed_config_fields",
            json!({ "stream": stream_id, "node": mixer, "exposed_config_fields": ["frame_width"],
                    "author": an_agent_named("alice") }),
        )
        .await
        .expect_err("a natively compiled node exposes no config field");
    assert_eq!(
        refused_exposure.refused_node.as_deref(),
        Some(mixer.as_str()),
        "{refused_exposure:?}"
    );
    assert!(
        refused_exposure
            .runtime_words
            .contains("node `mixer` exposes frame_width"),
        "{refused_exposure:?}"
    );
    // Exposing an input of a node type that takes no config is checked and records no commit.
    let exposed = the_edit_applied(
        &engine,
        "set_exposed_config_fields",
        json!({ "stream": stream_id, "node": monitor, "exposed_config_fields": ["video"] }),
    )
    .await;
    assert!(exposed.get("transaction").is_none(), "{exposed}");
    assert_eq!(exposed["operations"], json!([]), "{exposed}");
    let link = json!({ "stream": stream_id,
                       "source": { "node": SOURCE_NODE_UUID, "port": "video" },
                       "target": { "node": monitor, "port": "video" } });
    let connected = the_edit_applied(&engine, "connect", link.clone()).await;
    assert_eq!(connected["connected_links"][0]["stream"], stream_id);
    the_edit_applied(
        &engine,
        "rename_node",
        json!({ "stream": stream_id, "node": SOURCE_NODE_UUID, "name": "camera" }),
    )
    .await;
    the_edit_applied(
        &engine,
        "set_studio_fields",
        json!({ "stream": stream_id, "node": SOURCE_NODE_UUID,
                "studio": { "position": [120, 40] } }),
    )
    .await;
    let grouped = the_edit_applied(
        &engine,
        "add_group",
        json!({ "stream": stream_id, "title": "sources", "nodes": [SOURCE_NODE_UUID] }),
    )
    .await;
    let group = grouped["added_groups"][0]["group"].as_str().unwrap();
    the_edit_applied(
        &engine,
        "set_group_fields",
        json!({ "stream": stream_id, "group": group, "title": "cameras" }),
    )
    .await;
    the_edit_applied(
        &engine,
        "remove_group",
        json!({ "stream": stream_id, "group": group }),
    )
    .await;
    let noted = the_edit_applied(
        &engine,
        "add_note",
        json!({ "stream": stream_id, "text": "check the exposure",
                "position": [40, 300], "size": [240, 120] }),
    )
    .await;
    let note = noted["added_notes"][0]["note"].as_str().unwrap();
    the_edit_applied(
        &engine,
        "set_note_fields",
        json!({ "stream": stream_id, "note": note, "text": "check the focus" }),
    )
    .await;
    the_edit_applied(
        &engine,
        "remove_note",
        json!({ "stream": stream_id, "note": note }),
    )
    .await;
    the_edit_applied(&engine, "disconnect", link).await;
    the_edit_applied(
        &engine,
        "remove_node",
        json!({ "stream": stream_id, "node": monitor }),
    )
    .await;
    let transacted = the_edit_applied(
        &engine,
        "transact",
        json!({ "operations": [
            { "operation": "add_node", "stream": stream_id, "type": sink_type.as_str(),
              "name": "spare", "as": "spare" },
            { "operation": "connect", "stream": stream_id,
              "source": { "node": SOURCE_NODE_UUID, "port": "video" },
              "target": { "node": "spare", "port": "video" } },
        ] }),
    )
    .await;
    assert_eq!(transacted["added_nodes"][0]["stream"], stream_id);

    let kept = project.the_graph_document_of("main");
    assert_eq!(
        sorted_node_names_in(&kept),
        ["camera", "mixer", "spare"],
        "{kept}"
    );
    assert_eq!(
        links_by_node_name_in(&kept),
        ["camera/video -> spare/video"]
    );
    assert_eq!(
        super::input_value_set_tests::the_input_values_of(&kept, &mixer),
        json!({ "frame_width": 640, "gain": 0.25 })
    );
    let live = engine.the_live_graph_of(&stream_id);
    assert_eq!(
        sorted_node_names_in(&live),
        ["camera", "mixer", "spare"],
        "{live}"
    );
    // Every edit above commits once but the two `set_exposed_config_fields`: one refused, one
    // recording no commit.
    let recorded = the_actions_recorded_oldest_first(&project, 64);
    assert_eq!(recorded.len(), 17, "{recorded:?}");
    let actions: Vec<Value> = recorded
        .iter()
        .flat_map(|actions| actions.as_array().cloned().unwrap_or_default())
        .collect();
    assert_eq!(actions.len(), 18, "{actions:?}");
    for action in &actions {
        assert_eq!(action["stream"], stream_asset.to_string(), "{action}");
    }
}

/// An `add_node` sent by the stream id records the action the same `add_node` sent by the
/// stream asset's UUID records, and the undo of the one sent by id, made by a runtime that
/// loaded the project again, takes the node out.
#[tokio::test(flavor = "multi_thread")]
async fn an_edit_sent_by_id_records_the_action_sent_by_uuid_and_undoes_after_a_reload() {
    let project = a_project_whose_main_stream_holds_one_source();
    let stream_asset = project.stream_asset_of("main");
    let sink_type = StreamToolsTestSink::node_class_import_path();
    let add_the_monitor =
        |stream: Value| json!({ "stream": stream, "type": sink_type.as_str(), "name": "monitor" });
    {
        let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
        let stream_id = engine.a_stream_loaded_from_its_graph_document(&project, "main");
        the_edit_applied(&engine, "add_node", add_the_monitor(json!(stream_asset))).await;
        the_edit_applied(
            &engine,
            "undo",
            json!({ "project_directory": project.path() }),
        )
        .await;
        the_edit_applied(&engine, "add_node", add_the_monitor(json!(stream_id))).await;
    }
    let recorded = the_actions_recorded_oldest_first(&project, 3);
    assert_eq!(recorded.len(), 3, "{recorded:?}");
    assert_eq!(recorded[0], recorded[2], "{recorded:?}");
    assert_eq!(recorded[2][0]["stream"], stream_asset.to_string());

    let reloaded = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let reloaded_stream_id = reloaded.a_stream_loaded_from_its_graph_document(&project, "main");
    assert_eq!(
        sorted_node_names_in(&reloaded.the_live_graph_of(&reloaded_stream_id)),
        ["monitor", "source"]
    );
    the_edit_applied(
        &reloaded,
        "undo",
        json!({ "project_directory": project.path() }),
    )
    .await;
    assert_eq!(
        sorted_node_names_in(&project.the_graph_document_of("main")),
        ["source"]
    );
    assert_eq!(
        sorted_node_names_in(&reloaded.the_live_graph_of(&reloaded_stream_id)),
        ["source"]
    );
}

/// A stream id the runtime holds no stream under is refused with `stream_not_found`, at the
/// operation that names it, in words naming the streams the runtime holds; nothing is saved.
#[tokio::test(flavor = "multi_thread")]
async fn a_stream_id_no_stream_is_held_under_is_refused_naming_the_streams_held() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_whose_main_stream_holds_one_source();
    let stream_id = engine.a_stream_loaded_from_its_graph_document(&project, "main");
    let saved_before = project.the_graph_document_of("main");

    let refusal = engine
        .the_answer_or_the_refusal_of(
            "transact",
            json!({ "author": an_agent_named("alice"), "operations": [
                { "operation": "rename_node", "stream": stream_id,
                  "node": SOURCE_NODE_UUID, "name": "camera" },
                { "operation": "rename_node", "stream": "nowhere-0a1b2c",
                  "node": SOURCE_NODE_UUID, "name": "lens" },
            ] }),
        )
        .await
        .expect_err("a stream id no stream is held under is refused");
    assert_eq!(
        refusal.refusal_code,
        streamlib_runtime_client_contract::local_api_wire_contract::ControlOperationRefusalCode::StreamNotFound,
        "{refusal:?}"
    );
    assert_eq!(refusal.refused_operation_index, Some(1), "{refusal:?}");
    assert!(
        refusal.runtime_words.contains("`nowhere-0a1b2c`")
            && refusal.runtime_words.contains(&format!("`{stream_id}`")),
        "{refusal:?}"
    );
    assert_eq!(project.the_graph_document_of("main"), saved_before);
}

/// A transact refused for a stream id no stream is held under, beside one naming a held stream
/// by its id, and an edit sent by the stream asset's UUID refused for a blank idempotency key,
/// are each kept on the project's change feed with the operations as they were sent.
#[tokio::test(flavor = "multi_thread")]
async fn a_refused_edit_naming_its_stream_by_either_name_is_kept_on_the_change_feed() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_whose_main_stream_holds_one_source();
    let stream_id = engine.a_stream_loaded_from_its_graph_document(&project, "main");
    let stream_asset = project.stream_asset_of("main");
    let served = LocalApiServedOnAFreshSocket::over(engine.operations_on_the_loaded_streams());
    let connection = rmcp_client_over_the_upgraded_stream(&served).await;
    let transactions_resource_uri = tool_answer(
        &tool_call_over_the_connection(
            &connection,
            "list_assets",
            json!({ "project_directory": project.path() }),
        )
        .await,
    )
    .expect("the project's assets list")["transactions_resource_uri"]
        .as_str()
        .expect("the change feed's URI")
        .to_owned();

    let operations_by_id = json!([
        { "operation": "rename_node", "stream": stream_id,
          "node": SOURCE_NODE_UUID, "name": "camera" },
        { "operation": "rename_node", "stream": "nowhere-0a1b2c",
          "node": SOURCE_NODE_UUID, "name": "lens" },
    ]);
    let refused_by_id = tool_call_over_the_connection(
        &connection,
        "transact",
        json!({ "operations": operations_by_id, "author": an_agent_named("alice") }),
    )
    .await;
    assert_eq!(refused_by_id["isError"], true, "{refused_by_id}");
    let refused_for_its_key = tool_call_over_the_connection(
        &connection,
        "rename_node",
        json!({ "stream": stream_asset, "node": SOURCE_NODE_UUID, "name": "camera",
                "author": an_agent_named("bob"), "idempotency_key": "" }),
    )
    .await;
    assert_eq!(
        refused_for_its_key["isError"], true,
        "{refused_for_its_key}"
    );

    let read = connection
        .read_resource(rmcp::model::ReadResourceRequestParams::new(
            transactions_resource_uri,
        ))
        .await
        .expect("the change feed reads");
    let read = serde_json::to_value(read).unwrap();
    let feed: Value = serde_json::from_str(read["contents"][0]["text"].as_str().unwrap()).unwrap();
    let entries = feed["transactions"].as_array().expect("the feed's entries");
    let (newest, refused_before_it) = (&entries[0], &entries[1]);
    assert_eq!(newest["status"], "refused", "{feed}");
    assert_eq!(newest["author_name"], "bob", "{feed}");
    assert_eq!(
        newest["operations"],
        json!([{ "operation": "rename_node", "stream": stream_asset,
                 "node": SOURCE_NODE_UUID, "name": "camera" }])
    );
    assert_eq!(refused_before_it["status"], "refused", "{feed}");
    assert_eq!(refused_before_it["author_name"], "alice", "{feed}");
    assert_eq!(refused_before_it["operations"], operations_by_id);
    assert_eq!(
        refused_before_it["refusal"]["refusal_code"],
        "stream_not_found"
    );
}
