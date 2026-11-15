// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! List, stop and remove against an engine recording its streams: each stream listed by its id,
//! `open_viewer` and `set_stream_clock_scale` refused where they cannot act, the stream tools acting
//! on graph documents and loaded streams, and the records an earlier runtime wrote or a failed run
//! leaves.

use super::*;

impl AnEngineRecordingItsStreamsInATemporaryStateDirectory {
    /// Load `project`'s stream `stream_name` as its library host would, not started, under
    /// the id its location computes to, which it returns.
    fn a_stream_its_host_loaded_without_its_start(
        &self,
        project: &ProjectWithGraphDocuments,
        stream_name: &str,
    ) -> String {
        let stream_id = project.stream_id_of(stream_name);
        self.engine
            .load_stream_from_graph_snapshot(
                &GraphSnapshot::from_graph_document(the_graph_of_a_stream_named(stream_name))
                    .expect("the graph parses"),
                OptionsForLoadingOneStream::at_stream_location(
                    stream_id.clone(),
                    project.location_of(stream_name),
                )
                .in_stream_environment(project.stream_environment())
                .with_display_name(stream_name),
            )
            .expect("the host loads its stream");
        stream_id
    }

    /// Record `project`'s stream `stream_name` with its graph document, stopped when
    /// `stopped` says so, and not loaded; returns its id.
    fn a_stream_recorded_and_not_loaded(
        &self,
        project: &ProjectWithGraphDocuments,
        stream_name: &str,
        stopped: bool,
    ) -> String {
        project.write_the_graph_document(the_graph_of_a_stream_named(stream_name));
        let mut record = StreamRecord::of_a_running_stream(
            project.stream_id_of(stream_name),
            project.location_of(stream_name),
            &project.stream_environment(),
        );
        record.stopped = stopped;
        self.stream_records()
            .write(&record)
            .expect("the record is written");
        record.stream_id
    }
}

impl ProjectWithGraphDocuments {
    /// The location of this project's stream `stream_name` as the local API states it.
    fn location_on_the_wire_of(&self, stream_name: &str) -> Value {
        json!({
            "project_directory": self.path(),
            "project": self.project,
            "stream_asset": self.stream_asset_of(stream_name),
        })
    }
}

/// `list_streams` lists each stream by its id with its display name and
/// location, and every tool resolves a stream by that id exactly.
#[tokio::test(flavor = "multi_thread")]
async fn list_streams_lists_each_stream_by_id_and_every_tool_resolves_that_id_exactly() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = ProjectWithGraphDocuments::holding(the_graph_of_a_stream_named("live"));
    let live = engine.a_stream_its_host_loaded_without_its_start(&project, "live");
    let parked = engine.a_stream_recorded_and_not_loaded(&project, "parked", true);
    let waiting = engine.a_stream_recorded_and_not_loaded(&project, "waiting", false);

    let listed = tool_answer(
        &tool_call_result(
            engine.operations_on_the_loaded_streams(),
            "list_streams",
            json!({}),
        )
        .await,
    )
    .expect("list_streams answers");

    let location_of = |stream_name: &str| project.location_on_the_wire_of(stream_name);
    let mut expected_streams = vec![
        json!({ "stream": live, "display_name": "live", "location": location_of("live"), "state": "running", "node_count": 1, "capture_devices_whose_access_was_never_asked_for": [], "failed_because": null, "action": null, "step": null }),
        json!({ "stream": parked, "display_name": "parked", "location": location_of("parked"), "state": "stopped", "node_count": null, "capture_devices_whose_access_was_never_asked_for": [], "failed_because": null, "action": null, "step": null }),
        json!({ "stream": waiting, "display_name": "waiting", "location": location_of("waiting"), "state": "running", "node_count": null, "capture_devices_whose_access_was_never_asked_for": [], "failed_because": null, "action": null, "step": null }),
    ];
    expected_streams.sort_by_key(|listed| listed["stream"].as_str().unwrap().to_string());
    assert_eq!(listed, json!({ "streams": expected_streams }));

    let graph = first_text_block_json(
        &tool_call_result(
            engine.operations_on_the_loaded_streams(),
            "graph",
            json!({ "stream": live }),
        )
        .await,
    );
    assert_eq!(graph["stream"], live.as_str(), "{graph}");
    for not_the_id in ["live", live.to_uppercase().as_str()] {
        let refusal = tool_answer(
            &tool_call_result(
                engine.operations_on_the_loaded_streams(),
                "graph",
                json!({ "stream": not_the_id }),
            )
            .await,
        )
        .expect_err("a stream is named by its exact id");
        assert!(
            refusal.contains(not_the_id) && refusal.contains(&live),
            "{refusal}"
        );
    }
}

/// `open_viewer` is refused over `POST /mcp`, whose connection ends with its
/// answer, and over `/mcp/stdio` names a node that is not running; the
/// connection closes only a viewer it opened.
#[tokio::test(flavor = "multi_thread")]
async fn open_viewer_is_refused_over_post_and_for_a_node_not_running() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = ProjectWithGraphDocuments::holding(the_graph_of_a_stream_named("live"));
    let live = engine.a_stream_its_host_loaded_without_its_start(&project, "live");
    let graph = first_text_block_json(
        &tool_call_result(
            engine.operations_on_the_loaded_streams(),
            "graph",
            json!({ "stream": live }),
        )
        .await,
    );
    let source_id = graph["nodes"]
        .as_array()
        .expect("the graph lists its nodes")
        .iter()
        .find(|node| node["name"] == "source")
        .and_then(|node| node["id"].as_str())
        .expect("the graph lists the source node's id")
        .to_string();
    let the_ports_viewer = json!({ "stream": live, "node_id": source_id, "port": "video" });

    let over_post = tool_answer(
        &tool_call_result(
            engine.operations_on_the_loaded_streams(),
            "open_viewer",
            the_ports_viewer.clone(),
        )
        .await,
    )
    .expect_err("a `POST /mcp` request holds no viewer open");
    assert!(over_post.contains("/mcp/stdio"), "{over_post}");

    let served = LocalApiServedOnAFreshSocket::over(engine.operations_on_the_loaded_streams());
    let client = rmcp_client_over_the_upgraded_stream(&served).await;
    let not_running_result =
        tool_call_over_the_connection(&client, "open_viewer", the_ports_viewer.clone()).await;
    let not_running = tool_answer(&not_running_result)
        .expect_err("a node that is not running has nothing to view");
    assert!(
        not_running.contains("source") && not_running.contains("not running"),
        "{not_running}"
    );
    let refusal = &not_running_result["structuredContent"]["refusal"];
    assert_eq!(refusal["action"], "open_viewer", "{refusal}");
    assert_eq!(refusal["not_running_because"], "preparing", "{refusal}");
    assert_eq!(
        format!(
            "open_viewer failed: {}",
            refusal["runtime_words"].as_str().unwrap()
        ),
        not_running
    );

    engine
        .engine
        .loaded_stream_of_the_id(&live)
        .expect("the stream is loaded")
        .hold_stopped_without_a_start();
    let stopped_result =
        tool_call_over_the_connection(&client, "open_viewer", the_ports_viewer).await;
    assert_eq!(
        stopped_result["structuredContent"]["refusal"]["not_running_because"], "playback_stopped",
        "{stopped_result}"
    );
    let unknown = tool_answer(
        &tool_call_over_the_connection(
            &client,
            "close_viewer",
            json!({ "viewer": format!("{live}/{source_id}/video") }),
        )
        .await,
    )
    .expect_err("the connection opened no viewer");
    assert!(unknown.contains("no viewer"), "{unknown}");
}

/// `set_stream_clock_scale` reaches the named stream's clock, refusing a stream
/// whose clock is not running and a stream the runtime has not loaded.
#[tokio::test(flavor = "multi_thread")]
async fn set_stream_clock_scale_refuses_a_stream_with_no_clock_running() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = ProjectWithGraphDocuments::holding(the_graph_of_a_stream_named("live"));
    let live = engine.a_stream_its_host_loaded_without_its_start(&project, "live");

    let not_started = tool_answer(
        &tool_call_result(
            engine.operations_on_the_loaded_streams(),
            "set_stream_clock_scale",
            json!({ "stream": live, "scale": 0.5 }),
        )
        .await,
    )
    .expect_err("a stream that is not started has no clock to scale");
    assert!(
        not_started.contains(&format!("`{live}` is not started")),
        "{not_started}"
    );

    let not_loaded = tool_answer(
        &tool_call_result(
            engine.operations_on_the_loaded_streams(),
            "set_stream_clock_scale",
            json!({ "stream": "nowhere", "scale": 0.5 }),
        )
        .await,
    )
    .expect_err("a stream that is not loaded is refused");
    assert!(
        not_loaded.contains("nowhere") && not_loaded.contains(&live),
        "{not_loaded}"
    );
}

/// Stop and remove a project through the tools: each queues its action and
/// answers it at once, refuses what it cannot do naming why with nothing
/// queued, and its action leaves the state directory as it says.
#[tokio::test(flavor = "multi_thread")]
async fn stop_playback_and_remove_project_queue_actions_on_the_streams_the_runtime_holds() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = ProjectWithGraphDocuments::holding(the_graph_of_a_stream_named("live"));
    let live = engine.a_stream_its_host_loaded_without_its_start(&project, "live");
    let parked = engine.a_stream_recorded_and_not_loaded(&project, "parked", true);
    let call = |tool_name: &'static str, arguments: Value| {
        let operations_on_the_loaded_streams = engine.operations_on_the_loaded_streams();
        async move {
            tool_answer(
                &tool_call_result(operations_on_the_loaded_streams, tool_name, arguments).await,
            )
        }
    };

    let refusal = call(
        "stop_playback",
        json!({ "project_directory": "relative/project" }),
    )
    .await
    .expect_err("a relative project directory does not stop");
    assert!(refusal.contains("absolute"), "{refusal}");

    let stop = call(
        "stop_playback",
        json!({ "project_directory": project.path() }),
    )
    .await
    .unwrap();
    assert_eq!(
        stop["project_directory"],
        std::fs::canonicalize(project.path())
            .unwrap()
            .display()
            .to_string(),
        "{stop}"
    );
    let stopped = the_stream_action_once_finished(
        &engine.operations_on_the_loaded_streams(),
        stop["action"].as_str().unwrap(),
    )
    .await;
    assert_eq!(stopped["kind"], "stop", "{stopped}");
    assert_eq!(stopped["state"], "done", "{stopped}");
    assert_eq!(
        stopped["streams"].as_array().unwrap().len(),
        1,
        "the stopped `parked` is left out of the stop: {stopped}"
    );
    assert_eq!(stopped["streams"][0]["stream"], live.as_str(), "{stopped}");
    assert_eq!(stopped["streams"][0]["state"], "stopped", "{stopped}");
    assert!(engine.engine.ids_of_the_loaded_streams().is_empty());
    let refusal = call(
        "stop_playback",
        json!({ "project_directory": project.path() }),
    )
    .await
    .expect_err("a project none of whose streams runs does not stop again");
    assert!(refusal.contains("none to stop"), "{refusal}");

    let removal = call(
        "remove_project",
        json!({ "project_directory": project.path() }),
    )
    .await
    .unwrap();
    let removed = the_stream_action_once_finished(
        &engine.operations_on_the_loaded_streams(),
        removal["action"].as_str().unwrap(),
    )
    .await;
    assert_eq!(removed["kind"], "remove", "{removed}");
    assert_eq!(removed["state"], "done", "{removed}");
    assert_eq!(engine.current_record_of(&parked), None);
    let refusal = call(
        "remove_project",
        json!({ "project_directory": project.path() }),
    )
    .await
    .expect_err("a project the runtime no longer holds a stream of is refused");
    assert!(
        refusal.contains("holds no stream of the project") && refusal.contains("Loaded: none"),
        "{refusal}"
    );
}

/// `set_stream_display_name` writes a stream's display name in its graph document: a loaded
/// stream is shown under it at once and keeps its id, and a stream recorded and not loaded is
/// listed under it; `delete_stream` forgets a recorded stream, and `create_stream` in a
/// project with a loaded stream queues the load of the stream it created.
#[tokio::test(flavor = "multi_thread")]
async fn the_stream_tools_name_delete_and_create_a_projects_streams_in_their_graph_documents() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = ProjectWithGraphDocuments::holding(the_graph_of_a_stream_named("live"));
    let live = engine.a_stream_its_host_loaded_without_its_start(&project, "live");
    let parked = engine.a_stream_recorded_and_not_loaded(&project, "parked", true);
    let call = |tool_name: &'static str, arguments: Value| {
        let operations_on_the_loaded_streams = engine.operations_on_the_loaded_streams();
        async move {
            tool_answer(
                &tool_call_result(operations_on_the_loaded_streams, tool_name, arguments).await,
            )
        }
    };
    let shown_as = |listed: &Value| -> Vec<(String, String)> {
        listed["streams"]
            .as_array()
            .unwrap()
            .iter()
            .map(|listed| {
                (
                    listed["stream"].as_str().unwrap().to_owned(),
                    listed["display_name"].as_str().unwrap().to_owned(),
                )
            })
            .collect()
    };

    for (stream_name, display_name) in [("parked", "Hello camera"), ("live", "Desk mic")] {
        let named = call(
            "set_stream_display_name",
            json!({
                "project_directory": project.path(),
                "stream_asset": project.stream_asset_of(stream_name),
                "display_name": display_name,
            }),
        )
        .await
        .unwrap();
        assert_eq!(
            named["asset"]["asset"],
            json!(project.stream_asset_of(stream_name))
        );
        assert_eq!(named["asset"]["kind"], "stream");
    }
    let mut expected = vec![
        (live.clone(), "Desk mic".to_owned()),
        (parked.clone(), "Hello camera".to_owned()),
    ];
    expected.sort_unstable();
    assert_eq!(
        shown_as(&call("list_streams", json!({})).await.unwrap()),
        expected
    );

    call(
        "delete_stream",
        json!({
            "project_directory": project.path(),
            "stream_asset": project.stream_asset_of("parked"),
        }),
    )
    .await
    .unwrap();
    assert_eq!(
        shown_as(&call("list_streams", json!({})).await.unwrap()),
        vec![(live, "Desk mic".to_owned())]
    );
    assert!(engine.current_record_of(&parked).is_none());

    let created = call(
        "create_stream",
        json!({
            "project_directory": project.path(),
            "path": "streams/preview.graph.json",
            "display_name": "Preview",
        }),
    )
    .await
    .unwrap();
    assert_eq!(created["asset"]["path"], "streams/preview.graph.json");
    let load_of_the_created_stream = call(
        "describe_stream_action",
        json!({ "action": created["action"] }),
    )
    .await
    .unwrap();
    assert_eq!(
        load_of_the_created_stream["project_directory"],
        json!(project.path()),
        "a running project loads the stream it created: {load_of_the_created_stream}"
    );
}

/// An undo of `set_stream_display_name` shows the loaded stream under its former name again,
/// and an undo of `create_stream` unloads and forgets the stream the creation loaded.
#[tokio::test(flavor = "multi_thread")]
async fn undoing_a_stream_tool_brings_the_loaded_streams_back_in_line() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = ProjectWithGraphDocuments::holding(the_graph_of_a_stream_named("live"));
    let live = engine.a_stream_its_host_loaded_without_its_start(&project, "live");
    let call = |tool_name: &'static str, arguments: Value| {
        let operations_on_the_loaded_streams = engine.operations_on_the_loaded_streams();
        async move {
            tool_answer(
                &tool_call_result(operations_on_the_loaded_streams, tool_name, arguments).await,
            )
        }
    };
    let listed_stream_ids_and_names = || async {
        let listed = call("list_streams", json!({})).await.unwrap();
        listed["streams"]
            .as_array()
            .unwrap()
            .iter()
            .map(|listed| {
                (
                    listed["stream"].as_str().unwrap().to_owned(),
                    listed["display_name"].as_str().unwrap().to_owned(),
                )
            })
            .collect::<Vec<_>>()
    };
    let undo = || call("undo", json!({ "project_directory": project.path() }));

    call(
        "set_stream_display_name",
        json!({
            "project_directory": project.path(),
            "stream_asset": project.stream_asset_of("live"),
            "display_name": "Desk mic",
        }),
    )
    .await
    .unwrap();
    undo().await.expect("the display name change undoes");
    assert_eq!(
        listed_stream_ids_and_names().await,
        vec![(live.clone(), "live".to_owned())]
    );

    let created = call(
        "create_stream",
        json!({
            "project_directory": project.path(),
            "path": "streams/preview.graph.json",
            "display_name": "Preview",
        }),
    )
    .await
    .unwrap();
    the_stream_action_once_finished(
        &engine.operations_on_the_loaded_streams(),
        created["action"]
            .as_str()
            .expect("the creation queues a load"),
    )
    .await;
    assert_eq!(
        listed_stream_ids_and_names().await.len(),
        2,
        "the created stream is listed once its load ran"
    );
    undo().await.expect("the creation undoes");
    assert_eq!(
        listed_stream_ids_and_names().await,
        vec![(live, "live".to_owned())]
    );
}

/// A record an earlier runtime wrote is listed failed, keyed by its file
/// name, with why and how to run its stream again, until `remove_project` of
/// the project it names forgets it.
#[tokio::test(flavor = "multi_thread")]
async fn a_record_an_earlier_runtime_wrote_is_listed_failed_until_it_is_removed() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    std::fs::write(
        engine.streams_directory.join("main.json"),
        json!({
            "schema_version": 2,
            "stream_name": "main",
            "project_directory": "/home/someone/hello",
            "interpreter": "/home/someone/hello/.venv/bin/python",
            "stream_function": null,
            "graph": the_graph_of_a_stream_named("main"),
            "stopped": false,
            "failed_because": null,
            "runtime_crashes_in_a_row_implicating_it": 0,
            "exposure_rulings": [],
        })
        .to_string(),
    )
    .expect("the earlier runtime's record is written");
    let call = |tool_name: &'static str, arguments: Value| {
        let operations_on_the_loaded_streams = engine.operations_on_the_loaded_streams();
        async move {
            tool_answer(
                &tool_call_result(operations_on_the_loaded_streams, tool_name, arguments).await,
            )
        }
    };

    let listed = call("list_streams", json!({})).await.unwrap();
    let earlier = &listed["streams"][0];
    assert_eq!(earlier["stream"], "main", "{listed}");
    assert_eq!(earlier["state"], "failed", "{listed}");
    assert_eq!(earlier["location"], Value::Null, "{listed}");
    let failed_because = earlier["failed_because"].as_str().unwrap();
    assert!(
        failed_because.contains("an earlier runtime recorded it")
            && failed_because.contains("/home/someone/hello"),
        "{failed_because}"
    );
    let removal = call(
        "remove_project",
        json!({ "project_directory": "/home/someone/hello" }),
    )
    .await
    .unwrap();
    let removed = the_stream_action_once_finished(
        &engine.operations_on_the_loaded_streams(),
        removal["action"].as_str().unwrap(),
    )
    .await;
    assert_eq!(removed["state"], "done", "{removed}");
    assert_eq!(
        call("list_streams", json!({})).await.unwrap(),
        json!({ "streams": [] })
    );
}

/// A record an earlier runtime wrote, under the id a stream of `project_directory` computes to.
fn write_a_record_an_earlier_runtime_wrote(
    engine: &AnEngineRecordingItsStreamsInATemporaryStateDirectory,
    stream_id: &str,
    project_directory: &Path,
) {
    std::fs::write(
        engine.streams_directory.join(format!("{stream_id}.json")),
        json!({
            "schema_version": 2,
            "stream_name": "main",
            "project_directory": project_directory,
            "graph": the_graph_of_a_stream_named("main"),
            "stopped": false,
            "failed_because": null,
        })
        .to_string(),
    )
    .expect("the earlier runtime's record is written");
}

/// A run of a project whose stream id is held by a record an earlier runtime wrote from
/// that same project directory runs, as the record's `failed_because` says it will, and
/// its record replaces the earlier one.
#[tokio::test(flavor = "multi_thread")]
async fn a_run_of_a_project_an_earlier_runtime_recorded_replaces_that_record() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = ProjectWithGraphDocuments::holding(the_graph_of_main_with_a_node_uuid());
    let main_stream_id = project.stream_id_of("main");
    write_a_record_an_earlier_runtime_wrote(&engine, &main_stream_id, project.path());

    let run = the_run_of_the_project_once_finished(
        &engine.operations_on_the_loaded_streams(),
        project.path(),
    )
    .await;

    assert_eq!(run["state"], "done", "{run}");
    let ran = the_one_stream_of(&run);
    assert_eq!(ran["stream"], main_stream_id.as_str(), "{run}");
    let listing = the_listing_of(&engine.operations_on_the_loaded_streams(), &main_stream_id).await;
    assert_ne!(ran["state"], "failed", "{run} {listing}");
    assert_eq!(listing["failed_because"], Value::Null, "{listing}");
    assert_eq!(
        listing["location"],
        project.location_on_the_wire_of("main"),
        "{listing}"
    );
}

/// A record an earlier runtime wrote from another directory still holds its id against a
/// run, naming that directory.
#[tokio::test(flavor = "multi_thread")]
async fn a_record_an_earlier_runtime_wrote_from_another_directory_still_holds_its_id() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = ProjectWithGraphDocuments::holding(the_graph_of_a_stream_named("main"));
    let main_stream_id = project.stream_id_of("main");
    write_a_record_an_earlier_runtime_wrote(
        &engine,
        &main_stream_id,
        Path::new("/home/someone/elsewhere"),
    );

    let run = the_run_of_the_project_once_finished(
        &engine.operations_on_the_loaded_streams(),
        project.path(),
    )
    .await;

    assert_eq!(run["state"], "failed", "{run}");
    assert!(
        run["failed_because"]
            .as_str()
            .is_some_and(|why| why
                .contains("a record an earlier runtime wrote from /home/someone/elsewhere")),
        "{run}"
    );
}

/// On a runtime handed no uv, a run of a project with no venv records the project failed
/// saying the runtime unit carries no uv, and its action is done.
#[tokio::test(flavor = "multi_thread")]
async fn a_run_needing_a_venv_on_a_runtime_given_no_uv_records_it_failed_saying_so() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project_with_no_venv = ProjectWithGraphDocuments::with_no_venv();
    project_with_no_venv.write_the_graph_document(the_graph_of_a_stream_named("main"));
    let run = the_run_of_the_project_once_finished(
        &engine.operations_on_the_loaded_streams(),
        project_with_no_venv.path(),
    )
    .await;
    assert_eq!(run["state"], "done", "{run}");
    let project_failed = the_one_stream_of(&run);
    assert_eq!(project_failed["state"], "failed", "{run}");
    let listing = the_listing_of(
        &engine.operations_on_the_loaded_streams(),
        project_failed["stream"].as_str().unwrap(),
    )
    .await;
    assert!(
        listing["failed_because"]
            .as_str()
            .is_some_and(|failed_because| failed_because.contains("the runtime unit carries no uv")),
        "{listing}"
    );
    assert!(engine.engine.ids_of_the_loaded_streams().is_empty());
}

/// A run whose graph document does not load finishes done with the stream failed, listed
/// failed naming the document under the id its location computes to.
#[tokio::test(flavor = "multi_thread")]
async fn a_run_whose_graph_document_does_not_load_records_the_stream_failed_naming_it() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = ProjectWithGraphDocuments::holding(the_graph_of_a_stream_named("camera"));
    project.write_an_unloadable_graph_document("camera");
    let camera_stream_id = project.stream_id_of("camera");

    let run = the_run_of_the_project_once_finished(
        &engine.operations_on_the_loaded_streams(),
        project.path(),
    )
    .await;

    assert_eq!(run["state"], "done", "{run}");
    assert_eq!(
        run["streams"],
        json!([{
            "stream": camera_stream_id,
            "display_name": "camera",
            "state": "failed",
        }])
    );
    let listing = the_listing_of(
        &engine.operations_on_the_loaded_streams(),
        &camera_stream_id,
    )
    .await;
    assert_eq!(listing["state"], "failed");
    assert!(
        listing["failed_because"]
            .as_str()
            .is_some_and(|why| why.contains("`streams/camera.graph.json` does not load")),
        "{listing}"
    );
    assert_eq!(
        listing["location"],
        project.location_on_the_wire_of("camera")
    );
}

/// A run of a folder holding no `tatolab-project.json` fails its action saying so, and
/// records no stream.
#[tokio::test(flavor = "multi_thread")]
async fn a_run_of_a_folder_that_is_no_project_fails_its_action_saying_why() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let no_project = tempfile::tempdir().expect("a folder");

    let run = the_run_of_the_project_once_finished(
        &engine.operations_on_the_loaded_streams(),
        no_project.path(),
    )
    .await;

    assert_eq!(run["state"], "failed", "{run}");
    assert!(
        run["failed_because"]
            .as_str()
            .is_some_and(|why| why.contains("is not a project: it holds no tatolab-project.json")),
        "{run}"
    );
    assert_eq!(run["streams"], json!([]), "{run}");
    assert_eq!(
        tool_answer(
            &tool_call_result(
                engine.operations_on_the_loaded_streams(),
                "list_streams",
                json!({}),
            )
            .await,
        )
        .unwrap(),
        json!({ "streams": [] })
    );
}
