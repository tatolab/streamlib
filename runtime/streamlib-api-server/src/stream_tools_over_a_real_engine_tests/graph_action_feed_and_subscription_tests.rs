// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A graph action is the action its commit records, and what the control, project-change and
//! stream-graph subscribers are sent of it.

use super::*;

/// The graph actions a `transact` is sent, and the one action a single-operation tool is sent,
/// are what their commits record, field for field, once the mode, the author and the key that
/// carry them are set aside.
#[tokio::test(flavor = "multi_thread")]
async fn a_graph_action_sent_over_mcp_is_the_action_its_commit_records() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_whose_main_stream_holds_one_source();
    engine.a_stream_loaded_from_its_graph_document(&project, "main");
    let stream_asset = project.stream_asset_of("main");
    let sink_type = StreamToolsTestSink::node_class_import_path();
    let transacted_actions = json!([
        { "operation": "add_node", "stream": stream_asset, "type": sink_type.as_str(),
          "name": "monitor", "as": "monitor" },
        { "operation": "connect", "stream": stream_asset,
          "source": { "node": SOURCE_NODE_UUID, "port": "video" },
          "target": { "node": "monitor", "port": "video" } },
    ]);
    tool_answer(
        &tool_call_result(
            engine.operations_on_the_loaded_streams(),
            "transact",
            json!({
                "author": an_agent_named("alice"), "idempotency_key": "wire-monitor",
                "operations": transacted_actions,
            }),
        )
        .await,
    )
    .expect("the transaction applies");
    let rename = json!({ "stream": stream_asset, "node": SOURCE_NODE_UUID, "name": "camera" });
    let mut rename_sent = rename.clone();
    rename_sent["author"] = an_agent_named("alice");
    tool_answer(
        &tool_call_result(
            engine.operations_on_the_loaded_streams(),
            "rename_node",
            rename_sent,
        )
        .await,
    )
    .expect("the rename applies");

    let recorded_actions_oldest_first: Vec<Value> =
        streamlib_project_repository::ProjectRepository::open(project.path())
            .expect("the project opens")
            .applied_transactions_newest_first(2)
            .expect("history reads")
            .into_iter()
            .rev()
            .map(|applied| serde_json::to_value(&applied.transaction.actions).unwrap())
            .collect();
    let mut rename_as_recorded = rename;
    rename_as_recorded["operation"] = json!("rename_node");
    assert_eq!(
        recorded_actions_oldest_first,
        vec![transacted_actions, json!([rename_as_recorded])]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_control_stream_subscriber_is_sent_a_transact_another_client_made_and_both_doors_agree() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_whose_main_stream_holds_one_source();
    let stream_id = engine.a_stream_loaded_from_its_graph_document(&project, "main");
    let stream_asset = project.stream_asset_of("main");
    let served = crate::control_json_rpc_adapter::tests::ControlServedWithAnMcpClient::over(
        engine.operations_on_the_loaded_streams(),
    )
    .await;

    let mut subscriber = served.control_websocket().await;
    subscriber
        .send(
            json!({ "jsonrpc": "2.0", "id": 1, "method": "subscribe_project_changes",
                      "params": { "projects": [project.project] } }),
        )
        .await;
    assert_eq!(
        subscriber.next_message().await["result"]["projects"],
        json!([project.project])
    );

    let applied = tool_answer(
        &served
            .mcp_tool_result(
                "transact",
                json!({
                    "author": an_agent_named("alice"),
                    "operations": [{ "operation": "rename_node", "stream": stream_asset,
                                     "node": SOURCE_NODE_UUID, "name": "camera" }],
                }),
            )
            .await,
    )
    .expect("the transaction applies");

    let notification = subscriber.next_message_but_files_changed().await;
    assert_eq!(notification["method"], "project_changed", "{notification}");
    assert_eq!(notification["params"]["change"], "applied");
    assert_eq!(
        notification["params"]["transaction"]["transaction"],
        applied["transaction"]
    );
    assert_eq!(
        notification["params"]["transaction"]["author_name"],
        "alice"
    );

    let graph_over_mcp = tool_answer(
        &served
            .mcp_tool_result("graph", json!({ "stream": stream_id }))
            .await,
    )
    .expect("the graph exports");
    let graph_over_control = served
        .control_call("graph", json!({ "stream": stream_id }))
        .await;
    assert_eq!(
        graph_over_control["result"]["nodes"],
        graph_over_mcp["nodes"]
    );
}

/// An import added to a project with a `pyproject.toml` has its venv synced on the project's
/// stream action queue, off the answer, and the subscriber of its change feed is sent
/// `venv_made_current` once that is done.
///
/// Mental-revert: without `add_dependency` queueing the venv's making current, uv never runs and
/// no `venv_made_current` arrives; without the announcement, the wait times out.
#[tokio::test(flavor = "multi_thread")]
async fn an_added_import_has_the_venv_synced_and_its_subscriber_is_sent_venv_made_current() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let uv_directory = tempfile::tempdir().expect("a directory for the runtime unit's uv");
    let uv_runs = uv_directory.path().join("runs");
    let uv_making_the_venv = uv_directory.path().join("uv");
    write_an_executable_script_from_a_child_process(
        &uv_making_the_venv,
        &format!(
            "#!/bin/sh\necho \"$*\" >> '{runs}'\nmkdir -p .venv/bin && printf '#!/bin/sh\\nexit 0\\n' > .venv/bin/python && chmod 755 .venv/bin/python\n",
            runs = uv_runs.display()
        ),
    );
    engine
        .engine
        .set_runtime_unit_uv_executable(uv_making_the_venv)
        .expect("the runtime is handed its uv");
    let arm = a_project_whose_main_stream_holds_one_source();
    let cell = ProjectWithGraphDocuments::with_no_venv();
    cell.write_the_graph_document(the_graph_of_a_stream_named("main"));
    let served = crate::control_json_rpc_adapter::tests::ControlServedWithAnMcpClient::over(
        engine.operations_on_the_loaded_streams(),
    )
    .await;
    let mut subscriber = served.control_websocket().await;
    subscriber
        .send(
            json!({ "jsonrpc": "2.0", "id": 1, "method": "subscribe_project_changes",
                      "params": { "projects": [cell.project] } }),
        )
        .await;
    assert_eq!(
        subscriber.next_message().await["result"]["projects"],
        json!([cell.project])
    );

    tool_answer(
        &served
            .mcp_tool_result(
                "add_dependency",
                json!({ "project_directory": cell.path(), "source": arm.path().display().to_string(),
                        "author": an_agent_named("alice") }),
            )
            .await,
    )
    .expect("the arm is imported");

    let venv_made_current = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let notification = subscriber.next_message().await;
            if notification["params"]["change"] == "venv_made_current" {
                return notification;
            }
        }
    })
    .await
    .expect("the venv's making current is sent as venv_made_current");
    assert_eq!(venv_made_current["method"], "project_changed");
    assert_eq!(venv_made_current["params"]["project"], json!(cell.project));
    assert_eq!(
        std::fs::read_to_string(&uv_runs)
            .unwrap_or_default()
            .lines()
            .collect::<Vec<_>>(),
        ["lock", "sync"]
    );
    assert!(cell.interpreter().is_file());
}

#[tokio::test(flavor = "multi_thread")]
async fn read_project_changes_over_control_answers_the_change_feed_its_resource_renders() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_whose_main_stream_holds_one_source();
    engine.a_stream_loaded_from_its_graph_document(&project, "main");
    let stream_asset = project.stream_asset_of("main");
    let served = crate::control_json_rpc_adapter::tests::ControlServedWithAnMcpClient::over(
        engine.operations_on_the_loaded_streams(),
    )
    .await;
    let applied = tool_answer(
        &served
            .mcp_tool_result(
                "transact",
                json!({
                    "author": an_agent_named("alice"),
                    "operations": [{ "operation": "rename_node", "stream": stream_asset,
                                     "node": SOURCE_NODE_UUID, "name": "camera" }],
                }),
            )
            .await,
    )
    .expect("the transaction applies");

    let read_over_control = served
        .control_call(
            "read_project_changes",
            json!({ "project_directory": project.path() }),
        )
        .await;
    let change_feed = &read_over_control["result"];
    let resource_read = serde_json::to_value(
        served
            .mcp_client
            .read_resource(rmcp::model::ReadResourceRequestParams::new(
                crate::mcp_project_change_feed::project_transactions_resource_uri(project.project),
            ))
            .await
            .expect("the change feed's resource reads"),
    )
    .unwrap();
    let change_feed_over_mcp: Value =
        serde_json::from_str(resource_read["contents"][0]["text"].as_str().unwrap()).unwrap();

    assert_eq!(
        change_feed["project"],
        json!(project.project),
        "{read_over_control}"
    );
    assert_eq!(change_feed["transactions"][0]["status"], "applied");
    assert_eq!(
        change_feed["transactions"][0]["transaction"],
        applied["transaction"]
    );
    assert_eq!(change_feed["transactions"][0]["author_name"], "alice");
    assert_eq!(*change_feed, change_feed_over_mcp);
}

/// The params of the next `stream_graph_changed` that is not a sample of drops, raises and link
/// states, which a subscriber is sent on each stream's first sample.
async fn the_next_stream_graph_change_but_its_samples(
    subscriber: &mut crate::control_json_rpc_adapter::tests::ControlWebSocketMessages,
) -> Value {
    loop {
        let stream_graph_change =
            the_next_notification_under(subscriber, "stream_graph_changed").await;
        if stream_graph_change["change"] != "drops_raises_or_link_states_changed" {
            return stream_graph_change;
        }
    }
}

/// A subscriber to a loaded stream's graph is sent each edit another client makes, is sent
/// `missed` when the stream is unloaded and again when it is loaded once more, and follows it to
/// its new load; a subscriber to the stream actions is sent each change of the close and the
/// open.
///
/// Mental-revert: a subscription left on the first load's topic hears nothing of the edit after
/// the open, and the last wait below fails.
#[tokio::test(flavor = "multi_thread")]
async fn a_stream_graph_subscriber_is_sent_each_edit_and_follows_its_stream_to_its_next_load() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_whose_main_stream_holds_one_source();
    let operations_on_the_loaded_streams = engine.operations_on_the_loaded_streams();
    let opened =
        the_run_of_the_project_once_finished(&operations_on_the_loaded_streams, project.path())
            .await;
    assert_eq!(opened["streams"][0]["state"], "stopped", "{opened}");
    let stream_id = project.stream_id_of("main");
    let stream_asset = project.stream_asset_of("main");
    let sink_type = StreamToolsTestSink::node_class_import_path();
    let served = crate::control_json_rpc_adapter::tests::ControlServedWithAnMcpClient::over(
        Arc::clone(&operations_on_the_loaded_streams),
    )
    .await;

    let mut subscriber = served.control_websocket().await;
    subscriber
        .send(
            json!({ "jsonrpc": "2.0", "id": 1, "method": "subscribe_stream_graph_changes",
                      "params": { "streams": [stream_id] } }),
        )
        .await;
    assert_eq!(
        the_next_answer_but_its_notifications(&mut subscriber).await["result"]["streams"],
        json!([stream_id])
    );
    subscriber
        .send(
            json!({ "jsonrpc": "2.0", "id": 2, "method": "subscribe_stream_action_changes",
                      "params": {} }),
        )
        .await;
    assert_eq!(
        the_next_answer_but_its_notifications(&mut subscriber).await["result"],
        json!({ "actions_under_way": [] })
    );

    let added = served
        .control_call(
            "add_node",
            json!({ "stream": stream_asset, "type": sink_type.as_str(), "name": "monitor" }),
        )
        .await;
    assert!(added.get("result").is_some(), "{added}");
    assert_eq!(
        the_next_stream_graph_change_but_its_samples(&mut subscriber).await,
        json!({ "stream": stream_id, "change": "edited", "node": null, "node_state": null })
    );

    for (project_action, how_the_stream_graph_is_left) in [
        ("close_project", "unloaded"),
        ("open_project", "loaded again"),
    ] {
        let queued = served
            .control_call(
                project_action,
                json!({ "project_directory": project.path() }),
            )
            .await;
        let action = queued["result"]["action"]
            .as_str()
            .unwrap_or_else(|| panic!("{project_action} is queued: {queued}"))
            .to_owned();
        let finished =
            the_stream_action_once_finished(&operations_on_the_loaded_streams, &action).await;
        assert_eq!(finished["state"], "done", "{project_action}: {finished}");
        loop {
            let stream_action_change =
                the_next_notification_under(&mut subscriber, "stream_action_changed").await;
            assert_eq!(
                stream_action_change["change"], "changed",
                "{stream_action_change}"
            );
            if stream_action_change["action"] == action {
                break;
            }
        }
        loop {
            let stream_graph_change =
                the_next_notification_under(&mut subscriber, "stream_graph_changed").await;
            if stream_graph_change["change"] == "missed" {
                assert_eq!(
                    stream_graph_change["stream"], stream_id,
                    "the stream's graph {how_the_stream_graph_is_left}"
                );
                break;
            }
        }
    }

    let added_on_the_new_load = served
        .control_call(
            "add_node",
            json!({ "stream": stream_asset, "type": sink_type.as_str(), "name": "second-monitor" }),
        )
        .await;
    assert!(
        added_on_the_new_load.get("result").is_some(),
        "{added_on_the_new_load}"
    );
    loop {
        let stream_graph_change =
            the_next_notification_under(&mut subscriber, "stream_graph_changed").await;
        if stream_graph_change["change"] == "edited" {
            assert_eq!(stream_graph_change["stream"], stream_id);
            break;
        }
    }
}

/// A native source whose every `process()` raises the same words.
#[streamlib::sdk::node(execution = continuous(interval_ms = 5), output("video", family = Image))]
pub struct StreamToolsTestSourceWhoseProcessRaises;

/// What [`StreamToolsTestSourceWhoseProcessRaises`] raises.
const WHAT_THE_RAISING_SOURCE_RAISES: &str = "the test source raises on every process";

impl streamlib::sdk::nodes::ContinuousNode for StreamToolsTestSourceWhoseProcessRaises::Node {
    fn process(
        &mut self,
        _ctx: &streamlib::sdk::context::RuntimeContextLimitedAccess<'_>,
    ) -> Result<()> {
        Err(streamlib::sdk::error::Error::Runtime(
            WHAT_THE_RAISING_SOURCE_RAISES.to_owned(),
        ))
    }
}

/// A subscriber to a stream's graph is sent `drops_raises_or_link_states_changed` on the stream's
/// first sample, and again once a node's `process()` starts raising after playback starts, which no
/// graph event reports, and the graph it reads then carries the raise.
///
/// Mental-revert: without the sampling, the raise reaches a pushed client only on its next
/// unrelated change, and the wait below times out.
#[tokio::test(flavor = "multi_thread")]
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — a stream's start creates the engine's GPU context; run with --features hardware-tests"
)]
async fn a_stream_graph_subscriber_is_sent_a_raise_no_graph_event_reports() {
    static REGISTERED: std::sync::Once = std::sync::Once::new();
    REGISTERED
        .call_once(|| NODE_REGISTRY.register::<StreamToolsTestSourceWhoseProcessRaises::Node>());
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = ProjectWithGraphDocuments::holding(json!({
        "stream": "raising",
        "nodes": [{
            "id": SOURCE_NODE_UUID,
            "name": "source",
            "type": StreamToolsTestSourceWhoseProcessRaises::node_class_import_path().as_str(),
            "config": {},
        }],
        "links": [],
    }));
    let operations_on_the_loaded_streams = engine.operations_on_the_loaded_streams();
    let opened =
        the_run_of_the_project_once_finished(&operations_on_the_loaded_streams, project.path())
            .await;
    assert_eq!(opened["streams"][0]["state"], "stopped", "{opened}");
    let stream_id = project.stream_id_of("raising");
    let served = crate::control_json_rpc_adapter::tests::ControlServedWithAnMcpClient::over(
        Arc::clone(&operations_on_the_loaded_streams),
    )
    .await;
    let mut subscriber = served.control_websocket().await;
    subscriber
        .send(
            json!({ "jsonrpc": "2.0", "id": 1, "method": "subscribe_stream_graph_changes",
                      "params": { "streams": [stream_id] } }),
        )
        .await;
    assert_eq!(
        subscriber.next_message().await["result"]["streams"],
        json!([stream_id])
    );

    loop {
        let first_sample =
            the_next_notification_under(&mut subscriber, "stream_graph_changed").await;
        if first_sample["change"] == "drops_raises_or_link_states_changed" {
            break;
        }
    }

    let queued = served
        .control_call(
            "start_playback",
            json!({ "project_directory": project.path() }),
        )
        .await;
    let action = queued["result"]["action"]
        .as_str()
        .unwrap_or_else(|| panic!("start_playback is queued: {queued}"))
        .to_owned();
    let started = the_stream_action_once_finished(&operations_on_the_loaded_streams, &action).await;
    assert_eq!(started["state"], "done", "{started}");

    let graph_carrying_the_raise = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let stream_graph_change =
                the_next_notification_under(&mut subscriber, "stream_graph_changed").await;
            if stream_graph_change["change"] != "drops_raises_or_link_states_changed" {
                continue;
            }
            assert_eq!(
                stream_graph_change["stream"], stream_id,
                "{stream_graph_change}"
            );
            let graph = served
                .control_call("graph", json!({ "stream": stream_id }))
                .await;
            let raising_because =
                graph["result"]["nodes"][0]["components"]["process_raising_because"]
                    .as_str()
                    .unwrap_or_default();
            if raising_because.contains(WHAT_THE_RAISING_SOURCE_RAISES) {
                return graph;
            }
        }
    })
    .await;
    assert!(
        graph_carrying_the_raise.is_ok(),
        "no drops_raises_or_link_states_changed was followed by a graph carrying the raise"
    );
}

/// `subscribe_stream_graph_changes` is refused on a one-shot `POST /control`, for no stream, and
/// for a stream not loaded, and `subscribe_stream_action_changes` on a one-shot `POST /control`.
#[tokio::test(flavor = "multi_thread")]
async fn a_stream_subscription_is_refused_off_the_websocket_and_for_a_stream_not_loaded() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_project_whose_main_stream_holds_one_source();
    let stream_id = engine.a_stream_loaded_from_its_graph_document(&project, "main");
    let served = crate::control_json_rpc_adapter::tests::ControlServedWithAnMcpClient::over(
        engine.operations_on_the_loaded_streams(),
    )
    .await;

    for (method, params) in [
        (
            "subscribe_stream_graph_changes",
            json!({ "streams": [stream_id] }),
        ),
        ("subscribe_stream_action_changes", json!({})),
    ] {
        let refused = served.control_call(method, params).await;
        let refusal = refused["error"]["message"].as_str().unwrap_or_default();
        assert!(refusal.contains("/control/ws"), "{refused}");
    }

    let mut subscriber = served.control_websocket().await;
    for (id, streams, refusal_names) in [
        (1, json!([]), "names no stream".to_owned()),
        (2, json!(["no-such-stream"]), "no-such-stream".to_owned()),
    ] {
        subscriber
            .send(
                json!({ "jsonrpc": "2.0", "id": id, "method": "subscribe_stream_graph_changes",
                          "params": { "streams": streams } }),
            )
            .await;
        let refused = subscriber.next_message().await;
        assert_eq!(refused["id"], id, "{refused}");
        let refusal = refused["error"]["message"].as_str().unwrap_or_default();
        assert!(refusal.contains(&refusal_names), "{refused}");
    }
}
