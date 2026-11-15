// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! End to end on the engine's own run, where the start creates its GPU context: a played project
//! through stop, play, close and remove, and a run replacing a recorded location's stream.

use super::*;

impl ProjectWithGraphDocuments {
    /// Remove the graph document of the stream `stream_name`.
    fn remove_the_graph_document(&self, stream_name: &str) {
        std::fs::remove_file(
            self.path()
                .join("streams")
                .join(format!("{stream_name}.graph.json")),
        )
        .expect("the graph document is removed");
    }
}

/// [`the_run_of_the_project_once_finished`], the start or reload `start_or_reload_tool_name`
/// names queued over `connection`.
async fn the_run_over_the_connection_once_finished(
    start_or_reload_tool_name: &str,
    connection: &RunningService<RoleClient, ()>,
    operations_on_the_loaded_streams: &Arc<dyn OperationsOnTheStreamsLoadedInThisRuntime>,
    project_directory: &Path,
) -> Value {
    let queued = tool_answer(
        &tool_call_over_the_connection(
            connection,
            start_or_reload_tool_name,
            json!({ "project_directory": project_directory }),
        )
        .await,
    )
    .expect("a run of a project is queued");
    the_stream_action_once_finished(
        operations_on_the_loaded_streams,
        queued["action"].as_str().expect("the action's id"),
    )
    .await
}

/// A project opened and played over a `/mcp/stdio` connection is recorded and plays on once
/// the connection drops, and `stop_playback`, `start_playback`, `close_project` and
/// `remove_project` act on it through the tools.
#[tokio::test(flavor = "multi_thread")]
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — a stream's start creates the engine's GPU context; run with --features hardware-tests"
)]
async fn a_played_project_end_to_end_outlives_its_connection_through_stop_play_close_and_remove() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = ProjectWithGraphDocuments::holding(the_graph_of_a_stream_named("camera"));
    let camera_stream_id = project.stream_id_of("camera");
    let served = LocalApiServedOnAFreshSocket::over(engine.operations_on_the_loaded_streams());
    let client = rmcp_client_over_the_upgraded_stream(&served).await;

    let opened = the_run_over_the_connection_once_finished(
        "open_project",
        &client,
        &engine.operations_on_the_loaded_streams(),
        project.path(),
    )
    .await;
    assert_eq!(opened["streams"][0]["state"], "stopped", "{opened}");
    let run = the_run_over_the_connection_once_finished(
        "start_playback",
        &client,
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
            "state": "running",
        }])
    );
    assert!(
        engine.current_record_of(&camera_stream_id).is_some(),
        "the run is recorded"
    );

    drop(client);
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(
        engine.engine.ids_of_the_loaded_streams(),
        std::slice::from_ref(&camera_stream_id)
    );

    let call = |tool_name: &'static str, arguments: Value| {
        let operations_on_the_loaded_streams = engine.operations_on_the_loaded_streams();
        async move {
            tool_answer(
                &tool_call_result(operations_on_the_loaded_streams, tool_name, arguments).await,
            )
        }
    };
    let operations_on_the_loaded_streams = engine.operations_on_the_loaded_streams();
    for (tool_name, arguments, state_the_action_leaves) in [
        (
            "stop_playback",
            json!({ "project_directory": project.path() }),
            json!("stopped"),
        ),
        (
            "start_playback",
            json!({ "project_directory": project.path() }),
            json!("running"),
        ),
        (
            "close_project",
            json!({ "project_directory": project.path() }),
            Value::Null,
        ),
        (
            "remove_project",
            json!({ "project_directory": project.path() }),
            Value::Null,
        ),
    ] {
        let queued = call(tool_name, arguments).await.unwrap();
        let finished = the_stream_action_once_finished(
            &operations_on_the_loaded_streams,
            queued["action"].as_str().unwrap(),
        )
        .await;
        assert_eq!(finished["state"], "done", "{tool_name}: {finished}");
        if !state_the_action_leaves.is_null() {
            assert_eq!(
                finished["streams"][0]["state"], state_the_action_leaves,
                "{tool_name}: {finished}"
            );
        }
    }
    assert!(engine.engine.ids_of_the_loaded_streams().is_empty());
}

/// `tatolab dev`'s reload: a run of a project replaces its recorded stream under
/// the same id and another load, whichever connection carries it; a bad save
/// records the stream failed and unloads it; a graph document of another stream
/// loads it under its own id, and a run once a document is gone forgets its stream;
/// and closing every connection unloads nothing.
#[tokio::test(flavor = "multi_thread")]
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — a stream's start creates the engine's GPU context; run with --features hardware-tests"
)]
async fn a_run_of_a_recorded_location_replaces_its_stream_from_any_connection() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = ProjectWithGraphDocuments::holding(the_graph_of_a_stream_named("camera"));
    let served = LocalApiServedOnAFreshSocket::over(engine.operations_on_the_loaded_streams());
    let first_connection = rmcp_client_over_the_upgraded_stream(&served).await;
    let other_connection = rmcp_client_over_the_upgraded_stream(&served).await;
    let operations_on_the_loaded_streams = engine.operations_on_the_loaded_streams();
    let camera_stream_id = project.stream_id_of("camera");
    let stream_tag_loaded_as_camera = || {
        engine
            .engine
            .loaded_stream_of_the_id(&camera_stream_id)
            .unwrap()
            .stream_tag()
    };

    the_run_over_the_connection_once_finished(
        "open_project",
        &first_connection,
        &operations_on_the_loaded_streams,
        project.path(),
    )
    .await;
    the_run_over_the_connection_once_finished(
        "start_playback",
        &first_connection,
        &operations_on_the_loaded_streams,
        project.path(),
    )
    .await;
    let first_load = stream_tag_loaded_as_camera();

    project.write_an_unloadable_graph_document("camera");
    let bad_save = the_run_over_the_connection_once_finished(
        "reload_project",
        &first_connection,
        &operations_on_the_loaded_streams,
        project.path(),
    )
    .await;
    assert_eq!(
        the_one_stream_of(&bad_save)["state"],
        "failed",
        "{bad_save}"
    );
    assert!(
        the_listing_of(&operations_on_the_loaded_streams, &camera_stream_id).await["failed_because"]
            .as_str()
            .is_some_and(|why| why.contains("does not load")),
    );
    assert!(
        engine.engine.ids_of_the_loaded_streams().is_empty(),
        "a bad save unloads the running stream"
    );

    project.write_the_graph_document(the_graph_of_a_stream_named("camera"));
    let replaced = the_run_over_the_connection_once_finished(
        "reload_project",
        &other_connection,
        &operations_on_the_loaded_streams,
        project.path(),
    )
    .await;
    let replaced = the_one_stream_of(&replaced);
    assert_eq!(replaced["stream"], camera_stream_id.as_str());
    assert_eq!(replaced["state"], "running");
    assert_ne!(stream_tag_loaded_as_camera(), first_load);

    project.remove_the_graph_document("camera");
    project.write_the_graph_document(the_graph_of_a_stream_named("studio"));
    the_run_over_the_connection_once_finished(
        "reload_project",
        &first_connection,
        &operations_on_the_loaded_streams,
        project.path(),
    )
    .await;
    let studio_stream_id = project.stream_id_of("studio");
    assert_eq!(
        engine.engine.ids_of_the_loaded_streams(),
        std::slice::from_ref(&studio_stream_id)
    );
    let listed_ids: Vec<String> = engine
        .engine
        .list_streams()
        .into_iter()
        .map(|listing| listing.stream_id)
        .collect();
    assert_eq!(listed_ids, std::slice::from_ref(&studio_stream_id));

    drop(other_connection);
    drop(first_connection);
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(
        engine.engine.ids_of_the_loaded_streams(),
        [studio_stream_id]
    );
}
