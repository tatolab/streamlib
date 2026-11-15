// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Graph edits are transactions: kept across a restart, undone per author, read back from the
//! change feed when refused, and applied once under an idempotency key; and an interface node
//! added with no port or family takes the next free input name or its first link's family.

use super::*;

/// Five edits sent as one `transact` are one commit: the graph document keeps them, so a
/// fresh runtime loading it finds them, and one `undo` reverses all five, on the document and
/// on the running stream.
#[tokio::test(flavor = "multi_thread")]
async fn five_edits_in_one_transaction_are_kept_across_a_restart_and_one_undo_reverses_them() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_whose_main_stream_holds_one_source();
    let stream_id = engine.a_stream_loaded_from_its_graph_document(&project, "main");
    let stream_asset = project.stream_asset_of("main");
    let sink_type = StreamToolsTestSink::processor_class_import_path();

    let applied = tool_answer(
        &tool_call_result(
            engine.operations_on_the_loaded_streams(),
            "transact",
            json!({
                "author": an_agent_named("alice"),
                "operations": [
                    { "operation": "add_node", "stream": stream_asset, "type": sink_type.as_str(),
                      "name": "monitor", "as": "monitor" },
                    { "operation": "connect", "stream": stream_asset,
                      "source": { "node": SOURCE_NODE_UUID, "port": "video" },
                      "target": { "node": "monitor", "port": "video" } },
                    { "operation": "rename_node", "stream": stream_asset,
                      "node": SOURCE_NODE_UUID, "name": "camera" },
                    { "operation": "set_studio_fields", "stream": stream_asset,
                      "node": SOURCE_NODE_UUID, "studio": { "position": [120, 40] } },
                    { "operation": "add_node", "stream": stream_asset, "type": sink_type.as_str(),
                      "name": "spare" },
                ],
            }),
        )
        .await,
    )
    .expect("the transaction applies");
    assert_eq!(applied["mode"], "commit", "{applied}");
    assert!(applied["transaction"].is_string(), "{applied}");
    assert!(applied["commit"].is_string(), "{applied}");
    assert_eq!(applied["operations"].as_array().map(Vec::len), Some(5));

    let kept = project.the_graph_document_of("main");
    assert_eq!(
        sorted_node_names_in(&kept),
        ["camera", "monitor", "spare"],
        "{kept}"
    );
    assert_eq!(
        links_by_node_name_in(&kept),
        ["camera/video -> monitor/video"],
        "{kept}"
    );
    let camera = kept["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|node| node["id"] == SOURCE_NODE_UUID)
        .expect("the source keeps its UUID");
    assert_eq!(
        camera["studio"],
        json!({ "contract": 1, "position": [120, 40] })
    );

    let restarted = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let reloaded_stream_id = restarted.a_stream_loaded_from_its_graph_document(&project, "main");
    let reloaded = restarted.the_live_graph_of(&reloaded_stream_id);
    assert_eq!(
        sorted_node_names_in(&reloaded),
        ["camera", "monitor", "spare"]
    );
    assert_eq!(
        links_by_node_name_in(&reloaded),
        ["camera/video -> monitor/video"]
    );

    let undone = tool_answer(
        &tool_call_result(
            engine.operations_on_the_loaded_streams(),
            "undo",
            json!({ "project_directory": project.path(), "author": an_agent_named("alice") }),
        )
        .await,
    )
    .expect("the undo applies");
    assert_eq!(undone["undoes"], applied["transaction"], "{undone}");
    let after_the_undo = project.the_graph_document_of("main");
    assert_eq!(
        sorted_node_names_in(&after_the_undo),
        ["source"],
        "{after_the_undo}"
    );
    assert_eq!(links_by_node_name_in(&after_the_undo), Vec::<String>::new());
    let live = engine.the_live_graph_of(&stream_id);
    assert_eq!(sorted_node_names_in(&live), ["source"], "{live}");
    assert_eq!(links_by_node_name_in(&live), Vec::<String>::new(), "{live}");
}

/// A `GraphInput` added with no `port` takes the next free `input1`, `input2` …, numbered
/// against the Inputs the level already declares, those an earlier add of the same transaction
/// declared included.
#[tokio::test(flavor = "multi_thread")]
async fn a_graph_input_added_with_no_port_takes_the_next_free_input_name() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_whose_main_stream_holds_one_source();
    engine.a_stream_loaded_from_its_graph_document(&project, "main");
    let stream_asset = project.stream_asset_of("main");
    let add_a_records_graph_input = || {
        json!({ "operation": "add_node", "stream": stream_asset,
                "type": "tatolab.stream:GraphInput", "config": { "family": "records" } })
    };
    let add_the_graph_inputs = |operations: Vec<Value>| {
        tool_call_result(
            engine.operations_on_the_loaded_streams(),
            "transact",
            json!({ "author": an_agent_named("alice"), "operations": operations }),
        )
    };

    tool_answer(
        &add_the_graph_inputs(vec![
            add_a_records_graph_input(),
            add_a_records_graph_input(),
        ])
        .await,
    )
    .expect("both Inputs are added");
    tool_answer(&add_the_graph_inputs(vec![add_a_records_graph_input()]).await)
        .expect("a third Input is added");

    let kept = project.the_graph_document_of("main");
    let mut ports_declared: Vec<&str> = kept["nodes"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|node| node["type"] == "tatolab.stream:GraphInput")
        .filter_map(|node| node["config"]["port"].as_str())
        .collect();
    ports_declared.sort_unstable();
    assert_eq!(ports_declared, ["input1", "input2", "input3"], "{kept}");
}

/// A `GraphInput` or `GraphOutput` added with no family runs nothing and renders no family
/// until a link reaches it, then takes the family of the port linked to it, its config left as
/// drawn.
#[tokio::test(flavor = "multi_thread")]
async fn an_interface_node_added_with_no_family_takes_the_family_of_its_first_link() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_whose_main_stream_holds_one_source();
    let stream_id = engine.a_stream_loaded_from_its_graph_document(&project, "main");
    let stream_asset = project.stream_asset_of("main");
    let transact = |operations: Value| {
        tool_call_result(
            engine.operations_on_the_loaded_streams(),
            "transact",
            json!({ "author": an_agent_named("alice"), "operations": operations }),
        )
    };

    tool_answer(
        &transact(json!([
            { "operation": "add_node", "stream": stream_asset,
              "type": "tatolab.stream:GraphInput" },
            { "operation": "add_node", "stream": stream_asset,
              "type": "tatolab.stream:GraphOutput", "as": "output" },
        ]))
        .await,
    )
    .expect("interface nodes naming no family are added");
    let live = engine.the_live_graph_of(&stream_id);
    let waiting_input = the_node_named(&live, "graphinput");
    assert_eq!(
        waiting_input["components"]["graph_interface"],
        json!({ "direction": "input", "port": "input1", "family": null }),
        "{waiting_input}"
    );
    assert_eq!(waiting_input["config"], json!({ "port": "input1" }));

    let the_graph_output_in = |graph: &Value| -> Value {
        graph["nodes"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|node| node["type"] == "tatolab.stream:GraphOutput")
            .cloned()
            .expect("the graph holds the GraphOutput")
    };
    let output_node = the_graph_output_in(&live)["id"].clone();
    tool_answer(
        &transact(json!([
            { "operation": "connect", "stream": stream_asset,
              "source": { "node": SOURCE_NODE_UUID, "port": "video" },
              "target": { "node": output_node, "port": "value" } },
        ]))
        .await,
    )
    .expect("the source is linked into the output");
    let live = engine.the_live_graph_of(&stream_id);
    let linked_output = the_graph_output_in(&live);
    assert_eq!(
        linked_output["components"]["graph_interface"],
        json!({ "direction": "output", "port": "output1", "family": "image" }),
        "{linked_output}"
    );
    assert_eq!(linked_output["config"], json!({ "port": "output1" }));
}

/// Two authors' edits interleave, and each author's `undo` reverses only that author's own;
/// `redo` brings back what the author's undo reversed.
#[tokio::test(flavor = "multi_thread")]
async fn each_authors_undo_reverses_only_their_own_edits() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_whose_main_stream_holds_one_source();
    let stream_id = engine.a_stream_loaded_from_its_graph_document(&project, "main");
    let stream_asset = project.stream_asset_of("main");
    let sink_type = StreamToolsTestSink::processor_class_import_path();
    let call = |tool_name: &'static str, arguments: Value| {
        let operations_on_the_loaded_streams = engine.operations_on_the_loaded_streams();
        async move {
            tool_answer(
                &tool_call_result(operations_on_the_loaded_streams, tool_name, arguments).await,
            )
            .unwrap_or_else(|refusal| panic!("`{tool_name}` was refused: {refusal}"))
        }
    };
    let add_a_sink_as = |author: &str, name: &str| {
        json!({
            "stream": stream_asset, "type": sink_type.as_str(), "name": name,
            "author": an_agent_named(author),
        })
    };
    let undo_by = |author: &str| json!({ "project_directory": project.path(), "author": an_agent_named(author) });

    let alice_first = call("add_node", add_a_sink_as("alice", "alice-first")).await["added_nodes"]
        [0]["node"]
        .clone();
    call("add_node", add_a_sink_as("bob", "bob-first")).await;
    call(
        "rename_node",
        json!({
            "stream": stream_asset, "node": alice_first, "name": "alice-renamed",
            "author": an_agent_named("alice"),
        }),
    )
    .await;

    call("undo", undo_by("bob")).await;
    assert_eq!(
        sorted_node_names_in(&project.the_graph_document_of("main")),
        ["alice-renamed", "source"]
    );
    let alice_undo = call("undo", undo_by("alice")).await;
    assert_eq!(
        sorted_node_names_in(&project.the_graph_document_of("main")),
        ["alice-first", "source"]
    );
    assert_eq!(alice_undo.get("undo_state"), None, "{alice_undo}");
    let alice_redo = call("redo", undo_by("alice")).await;
    assert_eq!(
        sorted_node_names_in(&project.the_graph_document_of("main")),
        ["alice-renamed", "source"]
    );
    assert_eq!(alice_redo["undo_state"], "applied", "{alice_redo}");
    assert_eq!(
        sorted_node_names_in(&engine.the_live_graph_of(&stream_id)),
        ["alice-renamed", "source"],
        "undo and redo apply to the running stream as well"
    );

    let nothing_left = tool_answer(
        &tool_call_result(
            engine.operations_on_the_loaded_streams(),
            "undo",
            undo_by("bob"),
        )
        .await,
    )
    .expect_err("bob's one edit is already undone");
    assert!(nothing_left.contains("bob"), "{nothing_left}");
}

/// A graph edit the runtime refuses, from any connection, is announced to a listen on the
/// project's change feed and read back from it tagged `refused`, newest first above the
/// transactions history records, with its author, the operations it asked for, its mode, its key
/// and the runtime's refusal; history records nothing for it.
#[tokio::test(flavor = "multi_thread")]
async fn a_refused_graph_edit_is_read_back_from_the_project_change_feed() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_whose_main_stream_holds_one_source();
    engine.a_stream_loaded_from_its_graph_document(&project, "main");
    let stream_asset = project.stream_asset_of("main");
    let served = LocalApiServedOnAFreshSocket::over(engine.operations_on_the_loaded_streams());
    let editing_connection = rmcp_client_over_the_upgraded_stream(&served).await;
    let listening_connection = rmcp_client_over_the_upgraded_stream(&served).await;
    let listed = tool_answer(
        &tool_call_over_the_connection(
            &editing_connection,
            "list_assets",
            json!({ "project_directory": project.path() }),
        )
        .await,
    )
    .expect("the project's assets list");
    let transactions_resource_uri = listed["transactions_resource_uri"]
        .as_str()
        .expect("the change feed's URI")
        .to_owned();
    let history_before = streamlib_project_repository::ProjectRepository::open(project.path())
        .expect("the project opens")
        .applied_transactions_newest_first(8)
        .expect("history reads");
    let mut subscription = listening_connection
        .listen(
            rmcp::model::SubscriptionFilter::builder()
                .resource_subscription(transactions_resource_uri.clone())
                .build(),
        )
        .await
        .expect("the runtime acknowledges a listen on the change feed");

    let absent_node = "33333333-3333-4333-8333-333333333333";
    let refused = tool_call_over_the_connection(
        &editing_connection,
        "remove_node",
        json!({
            "stream": stream_asset, "node": absent_node, "author": an_agent_named("alice"),
            "idempotency_key": "remove-gesture",
        }),
    )
    .await;
    assert_eq!(refused["isError"], true, "{refused}");
    let live_refused = tool_call_over_the_connection(
        &editing_connection,
        "rename_node",
        json!({
            "stream": stream_asset, "node": absent_node, "name": "glow", "mode": "live",
            "author": an_agent_named("bob"),
        }),
    )
    .await;
    assert_eq!(live_refused["isError"], true, "{live_refused}");

    let update = tokio::time::timeout(Duration::from_secs(5), subscription.next())
        .await
        .expect("the refusal is announced")
        .expect("the listen is still open")
        .expect("the update is well formed");
    let rmcp::model::ServerNotification::ResourceUpdatedNotification(update) = update else {
        panic!("expected notifications/resources/updated, got {update:?}");
    };
    assert_eq!(update.params.uri, transactions_resource_uri);

    let read = listening_connection
        .read_resource(rmcp::model::ReadResourceRequestParams::new(
            transactions_resource_uri.clone(),
        ))
        .await
        .expect("the change feed reads");
    let read = serde_json::to_value(read).unwrap();
    let feed: Value = serde_json::from_str(read["contents"][0]["text"].as_str().unwrap()).unwrap();
    let entries = feed["transactions"].as_array().expect("the feed's entries");
    assert_eq!(entries.len(), history_before.len() + 2, "{feed}");
    let (newest, refused_before_it) = (&entries[0], &entries[1]);
    assert_eq!(newest["status"], "refused", "{feed}");
    assert_eq!(newest["mode"], "live");
    assert_eq!(newest["author_name"], "bob");
    assert_eq!(newest["refusal"]["action"], "rename_node");
    assert_eq!(refused_before_it["status"], "refused");
    assert_eq!(refused_before_it["author_kind"], "agent");
    assert_eq!(refused_before_it["author_name"], "alice");
    assert_eq!(refused_before_it["mode"], "commit");
    assert_eq!(refused_before_it["idempotency_key"], "remove-gesture");
    assert_eq!(
        refused_before_it["operations"],
        json!([{ "operation": "remove_node", "stream": stream_asset, "node": absent_node }])
    );
    assert_eq!(
        refused_before_it["refusal"], refused["structuredContent"]["refusal"],
        "the feed carries the refusal the caller was answered"
    );
    assert!(
        refused_before_it["refused_at"]
            .as_str()
            .is_some_and(|at| at.ends_with('Z'))
    );
    assert!(
        entries[2..]
            .iter()
            .all(|entry| entry["status"] == "applied"),
        "{feed}"
    );
    assert_eq!(
        streamlib_project_repository::ProjectRepository::open(project.path())
            .expect("the project opens")
            .applied_transactions_newest_first(8)
            .expect("history reads"),
        history_before,
        "history records no refusal"
    );
}

/// A committed edit sent twice under one idempotency key, as a client retrying after a timeout
/// would, applies once to the running stream and its graph document and answers the first
/// transaction; its commit records the format version and the key. An undo sent twice under
/// its own key undoes once.
#[tokio::test(flavor = "multi_thread")]
async fn an_edit_sent_again_under_its_idempotency_key_applies_once() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_whose_main_stream_holds_one_source();
    let stream_id = engine.a_stream_loaded_from_its_graph_document(&project, "main");
    let stream_asset = project.stream_asset_of("main");
    let sink_type = StreamToolsTestSink::processor_class_import_path();
    let call = |tool_name: &'static str, arguments: Value| {
        let operations_on_the_loaded_streams = engine.operations_on_the_loaded_streams();
        async move {
            tool_answer(
                &tool_call_result(operations_on_the_loaded_streams, tool_name, arguments).await,
            )
            .unwrap_or_else(|refusal| panic!("`{tool_name}` was refused: {refusal}"))
        }
    };
    let add_the_sink = json!({
        "stream": stream_asset, "type": sink_type.as_str(), "name": "sink",
        "author": an_agent_named("alice"), "idempotency_key": "add-sink-gesture",
    });

    let first = call("add_node", add_the_sink.clone()).await;
    let retried = call("add_node", add_the_sink).await;
    assert_eq!(retried["transaction"], first["transaction"], "{retried}");
    assert_eq!(retried["commit"], first["commit"], "{retried}");
    assert_eq!(
        retried["added_nodes"][0]["node"],
        first["added_nodes"][0]["node"]
    );
    assert_eq!(
        sorted_node_names_in(&project.the_graph_document_of("main")),
        ["sink", "source"]
    );
    assert_eq!(
        sorted_node_names_in(&engine.the_live_graph_of(&stream_id)),
        ["sink", "source"],
        "the retry reached the running stream not at all"
    );

    let undo = json!({
        "project_directory": project.path(), "author": an_agent_named("alice"),
        "idempotency_key": "undo-gesture",
    });
    let undone = call("undo", undo.clone()).await;
    let undone_again = call("undo", undo).await;
    assert_eq!(undone_again["transaction"], undone["transaction"]);
    assert_eq!(
        sorted_node_names_in(&project.the_graph_document_of("main")),
        ["source"]
    );

    let recorded = streamlib_project_repository::ProjectRepository::open(project.path())
        .expect("the project opens")
        .applied_transactions_newest_first(2)
        .expect("history reads");
    assert_eq!(recorded.len(), 2, "one add and one undo, each once");
    assert_eq!(
        recorded[1].transaction.format_version,
        streamlib_project_repository::PROJECT_TRANSACTION_FORMAT_VERSION
    );
    assert_eq!(
        recorded[1]
            .transaction
            .idempotency_key
            .as_ref()
            .map(|key| key.as_str()),
        Some("add-sink-gesture")
    );
    assert_eq!(undone["format_version"], json!(2), "{undone}");
    assert_eq!(undone["idempotency_key"], "undo-gesture", "{undone}");
    assert!(
        undone["applied_at"]
            .as_str()
            .is_some_and(|applied_at| applied_at.ends_with('Z')),
        "{undone}"
    );
}
