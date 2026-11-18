// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! `create_project`, `open_project` and `cancel_stream_action`, and the project actions the runtime
//! refuses or abandons.

use super::*;

/// A project directory holding only a venv, so `create_project` writes every file of a
/// new project.
fn a_directory_for_create_project() -> ProjectWithGraphDocuments {
    let project = ProjectWithGraphDocuments::holding(the_graph_of_a_stream_named("main"));
    std::fs::remove_dir_all(project.path().join("streams")).unwrap();
    std::fs::remove_file(project.path().join("tatolab-project.json")).unwrap();
    std::fs::remove_file(project.path().join("pyproject.toml")).unwrap();
    project
}

/// `create_project` named a directory: what it answers, and the action once finished.
async fn create_project_in(
    operations_on_the_loaded_streams: &Arc<dyn OperationsOnTheStreamsLoadedInThisRuntime>,
    project_directory: &Path,
) -> (Value, Value) {
    let queued = tool_answer(
        &tool_call_result(
            Arc::clone(operations_on_the_loaded_streams),
            "create_project",
            json!({ "project_directory": project_directory }),
        )
        .await,
    )
    .expect("a create of a project is queued");
    let action = queued["action"]
        .as_str()
        .expect("the action's id")
        .to_string();
    let finished = the_stream_action_once_finished(operations_on_the_loaded_streams, &action).await;
    (queued, finished)
}

/// `create_project` in a named directory writes a new project's files and loads its
/// `streams/main.graph.json` under the folder's name, answering the stream's id before it
/// runs.
#[tokio::test(flavor = "multi_thread")]
async fn create_project_in_a_directory_writes_the_project_and_runs_its_stream_under_the_folders_name()
 {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let operations_on_the_loaded_streams = &engine.operations_on_the_loaded_streams();
    let project = a_directory_for_create_project();
    let folder_name = project
        .path()
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();

    let (queued, finished) =
        create_project_in(operations_on_the_loaded_streams, project.path()).await;

    let stream_id = queued["stream"]
        .as_str()
        .expect("the stream's id")
        .to_string();
    assert_eq!(
        queued,
        json!({
            "action": finished["action"],
            "stream": stream_id,
            "project_directory": project.path(),
        })
    );
    assert_eq!(finished["state"], "done", "{finished}");
    let [created_stream] = finished["streams"].as_array().unwrap().as_slice() else {
        panic!("the create took in one stream: {finished}");
    };
    assert_eq!(
        (&created_stream["stream"], &created_stream["display_name"]),
        (&json!(stream_id), &json!(folder_name)),
        "{finished}"
    );
    for file_a_new_project_starts_with in [
        "tatolab-project.json",
        "streams/main.graph.json",
        "pyproject.toml",
        ".python-version",
        ".gitignore",
    ] {
        assert!(
            project
                .path()
                .join(file_a_new_project_starts_with)
                .is_file(),
            "{file_a_new_project_starts_with}"
        );
    }
    assert!(engine.current_record_of(&stream_id).is_some());
    let main_graph_document: Value = serde_json::from_slice(
        &std::fs::read(project.path().join("streams/main.graph.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(main_graph_document["display_name"], json!(folder_name));
}

/// `create_project` over a directory already holding a file a new project starts with
/// fails its action with nothing written.
#[tokio::test(flavor = "multi_thread")]
async fn create_project_over_a_file_a_new_project_starts_with_fails_its_action_with_nothing_written()
 {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let operations_on_the_loaded_streams = &engine.operations_on_the_loaded_streams();
    let project = a_directory_for_create_project();
    std::fs::create_dir_all(project.path().join("streams")).unwrap();
    std::fs::write(
        project.path().join("streams/main.graph.json"),
        "# the user's own\n",
    )
    .unwrap();

    let (_queued, finished) =
        create_project_in(operations_on_the_loaded_streams, project.path()).await;

    assert_eq!(finished["state"], "failed", "{finished}");
    assert!(
        finished["failed_because"]
            .as_str()
            .is_some_and(|why| why.contains("already has streams/main.graph.json")),
        "{finished}"
    );
    assert!(!project.path().join("pyproject.toml").exists());
    assert_eq!(
        std::fs::read_to_string(project.path().join("streams/main.graph.json")).unwrap(),
        "# the user's own\n"
    );
}

/// `create_project` naming a sample the runtime unit does not carry is refused at once, naming
/// the samples it does, and `list_samples` lists none for a unit that carries none.
#[tokio::test(flavor = "multi_thread")]
async fn create_project_from_a_sample_the_unit_does_not_carry_is_refused_naming_what_it_carries() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let project = a_directory_for_create_project();

    let refusal = tool_answer(
        &tool_call_result(
            engine.operations_on_the_loaded_streams(),
            "create_project",
            json!({ "project_directory": project.path(), "sample": "camera-halftone" }),
        )
        .await,
    )
    .expect_err("a sample the unit does not carry is refused");

    assert!(
        refusal.contains("the runtime unit carries no sample `camera-halftone`; it carries none"),
        "{refusal}"
    );
    assert!(!project.path().join("tatolab-project.json").exists());
    assert_eq!(
        tool_answer(
            &tool_call_result(
                engine.operations_on_the_loaded_streams(),
                "list_samples",
                json!({}),
            )
            .await,
        )
        .expect("the samples are listed"),
        json!({ "samples": [] })
    );
}

/// `create_project` with a relative directory is refused, nothing created.
#[tokio::test(flavor = "multi_thread")]
async fn create_project_with_a_relative_directory_is_refused() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let refusal = tool_answer(
        &tool_call_result(
            engine.operations_on_the_loaded_streams(),
            "create_project",
            json!({ "project_directory": "a-relative-project" }),
        )
        .await,
    )
    .expect_err("a relative directory is refused");
    assert!(refusal.contains("must be an absolute path"), "{refusal}");
    assert!(!Path::new("a-relative-project").exists());
}

/// `open_project` answers its action at once, and the action is followed to `done` with the
/// stream of the graph document that does not load failed.
#[tokio::test(flavor = "multi_thread")]
async fn open_project_queues_an_open_followed_by_describe_stream_action() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let operations_on_the_loaded_streams = &engine.operations_on_the_loaded_streams();
    let project = ProjectWithGraphDocuments::holding(the_graph_of_a_stream_named("camera"));
    project.write_an_unloadable_graph_document("camera");
    let project_directory = project.path().to_path_buf();

    let queued = tool_answer(
        &tool_call_result(
            Arc::clone(operations_on_the_loaded_streams),
            "open_project",
            json!({ "project_directory": project.path() }),
        )
        .await,
    )
    .expect("a run of a project is queued");
    let action = queued["action"]
        .as_str()
        .expect("the action's id")
        .to_string();
    assert_eq!(queued["project_directory"], json!(project_directory));

    let finished = the_stream_action_once_finished(operations_on_the_loaded_streams, &action).await;

    assert_eq!(
        finished,
        json!({
            "action": action,
            "project_directory": project_directory,
            "kind": "open",
            "state": "done",
            "step": null,
            "venv_sync": null,
            "streams": [{
                "stream": project.stream_id_of("camera"),
                "display_name": "camera",
                "state": "failed",
            }],
            "failed_because": null,
        })
    );
}

/// A run cancelled while the runtime's uv makes its venv answers `in_progress`, has uv
/// killed and ends `cancelled` with its stream never loaded; a second cancel is refused,
/// the action finished.
#[tokio::test(flavor = "multi_thread")]
async fn cancel_stream_action_stops_a_running_venv_sync_and_the_action_ends_cancelled() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let operations_on_the_loaded_streams = &engine.operations_on_the_loaded_streams();
    let project = ProjectWithGraphDocuments::with_no_venv();
    project.write_the_graph_document(the_graph_of_a_stream_named("camera"));
    let uv_directory = tempfile::tempdir().expect("a directory for the runtime unit's uv");
    let uv_that_never_finishes = uv_directory.path().join("uv");
    write_an_executable_script_from_a_child_process(
        &uv_that_never_finishes,
        "#!/bin/sh
exec sleep 30
",
    );
    engine
        .engine
        .set_runtime_unit_uv_executable(uv_that_never_finishes)
        .expect("the runtime is handed its uv");
    let queued = tool_answer(
        &tool_call_result(
            Arc::clone(operations_on_the_loaded_streams),
            "open_project",
            json!({ "project_directory": project.path() }),
        )
        .await,
    )
    .expect("a run of a project is queued");
    let action = queued["action"]
        .as_str()
        .expect("the action's id")
        .to_string();
    the_stream_action_once(operations_on_the_loaded_streams, &action, |described| {
        described["step"] == "making_the_venv"
    })
    .await;
    let asked_at = std::time::Instant::now();

    let cancel = tool_answer(
        &tool_call_result(
            Arc::clone(operations_on_the_loaded_streams),
            "cancel_stream_action",
            json!({ "action": action }),
        )
        .await,
    )
    .expect("a running load takes its cancel");
    let finished = the_stream_action_once_finished(operations_on_the_loaded_streams, &action).await;

    assert_eq!(cancel, json!({ "action": action, "state": "in_progress" }));
    assert_eq!(
        (
            &finished["state"],
            &finished["step"],
            &finished["streams"],
            &finished["failed_because"]
        ),
        (
            &json!("cancelled"),
            &Value::Null,
            &json!([{
                "stream": project.stream_id_of("camera"),
                "display_name": "camera",
                "state": null,
            }]),
            &Value::Null
        ),
        "{finished}"
    );
    assert!(
        asked_at.elapsed() < Duration::from_secs(10),
        "the sync was waited for, not killed"
    );
    let again = tool_answer(
        &tool_call_result(
            Arc::clone(operations_on_the_loaded_streams),
            "cancel_stream_action",
            json!({ "action": action }),
        )
        .await,
    )
    .unwrap_err();
    assert!(again.contains("is already cancelled"), "{again}");
}

/// A graph document deleted while its project's load makes the venv is left out when the load
/// starts its streams: no record of its stream is written, and the project's other stream is.
#[tokio::test(flavor = "multi_thread")]
async fn a_graph_document_deleted_while_its_load_makes_the_venv_never_records_its_stream() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let operations_on_the_loaded_streams = &engine.operations_on_the_loaded_streams();
    let project = ProjectWithGraphDocuments::with_no_venv();
    project.write_the_graph_document(the_graph_of_a_stream_named("camera"));
    project.write_the_graph_document(the_graph_of_a_stream_named("screen"));
    let uv_directory = tempfile::tempdir().expect("a directory for the runtime unit's uv");
    let uv_released = uv_directory.path().join("released");
    let uv_held_until_released = uv_directory.path().join("uv");
    write_an_executable_script_from_a_child_process(
        &uv_held_until_released,
        &format!(
            "#!/bin/sh
while [ ! -e '{}' ]; do sleep 0.05; done
exit 1
",
            uv_released.display()
        ),
    );
    engine
        .engine
        .set_runtime_unit_uv_executable(uv_held_until_released)
        .expect("the runtime is handed its uv");
    let queued = tool_answer(
        &tool_call_result(
            Arc::clone(operations_on_the_loaded_streams),
            "open_project",
            json!({ "project_directory": project.path() }),
        )
        .await,
    )
    .expect("a run of a project is queued");
    let action = queued["action"]
        .as_str()
        .expect("the action's id")
        .to_string();
    the_stream_action_once(operations_on_the_loaded_streams, &action, |described| {
        described["step"] == "making_the_venv"
    })
    .await;

    std::fs::remove_file(project.path().join("streams/screen.graph.json"))
        .expect("the screen stream's graph document is deleted");
    std::fs::write(&uv_released, "").expect("uv is released");
    the_stream_action_once_finished(operations_on_the_loaded_streams, &action).await;

    let listed = tool_answer(
        &tool_call_result(
            Arc::clone(operations_on_the_loaded_streams),
            "list_streams",
            json!({}),
        )
        .await,
    )
    .expect("list_streams answers");
    let listed_stream_ids: Vec<&str> = listed["streams"]
        .as_array()
        .expect("the streams listed")
        .iter()
        .map(|listed_stream| listed_stream["stream"].as_str().expect("a stream id"))
        .collect();
    assert_eq!(
        listed_stream_ids,
        vec![project.stream_id_of("camera").as_str()],
        "{listed}"
    );
}

/// While its project's open makes the venv, a stream is not loaded, yet `graph` answers it from
/// its graph document and `transact` edits that document; the load that follows takes the
/// edit in.
#[tokio::test(flavor = "multi_thread")]
async fn a_stream_whose_project_is_preparing_is_read_and_edited_from_its_graph_document() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let operations_on_the_loaded_streams = &engine.operations_on_the_loaded_streams();
    register_the_stream_tools_test_sink_once();
    let project = ProjectWithGraphDocuments::with_no_venv();
    let mut graph = the_graph_of_a_stream_named("main");
    graph["nodes"][0]["id"] = json!(SOURCE_NODE_UUID);
    project.write_the_graph_document(graph);
    let main = project.stream_id_of("main");
    let uv_directory = tempfile::tempdir().expect("a directory for the runtime unit's uv");
    let uv_released = uv_directory.path().join("released");
    let uv_held_until_released = uv_directory.path().join("uv");
    write_an_executable_script_from_a_child_process(
        &uv_held_until_released,
        &format!(
            "#!/bin/sh
while [ ! -e '{}' ]; do sleep 0.05; done
exit 1
",
            uv_released.display()
        ),
    );
    engine
        .engine
        .set_runtime_unit_uv_executable(uv_held_until_released)
        .expect("the runtime is handed its uv");
    let queued = tool_answer(
        &tool_call_result(
            Arc::clone(operations_on_the_loaded_streams),
            "open_project",
            json!({ "project_directory": project.path() }),
        )
        .await,
    )
    .expect("an open of the project is queued");
    let action = queued["action"]
        .as_str()
        .expect("the action's id")
        .to_string();
    the_stream_action_once(operations_on_the_loaded_streams, &action, |described| {
        described["step"] == "making_the_venv"
    })
    .await;

    let read_while_preparing = tool_answer(
        &tool_call_result(
            Arc::clone(operations_on_the_loaded_streams),
            "graph",
            json!({ "stream": main }),
        )
        .await,
    )
    .expect("a stream preparing answers its graph from its document");
    assert_eq!(
        sorted_node_names_in(&read_while_preparing),
        ["source"],
        "{read_while_preparing}"
    );
    let applied = tool_answer(
        &tool_call_result(
            Arc::clone(operations_on_the_loaded_streams),
            "transact",
            json!({
                "author": an_agent_named("alice"),
                "operations": [
                    { "operation": "add_node", "stream": project.stream_asset_of("main"),
                      "type": StreamToolsTestSink::node_class_import_path().as_str(),
                      "name": "monitor" },
                ],
            }),
        )
        .await,
    )
    .expect("an edit of a stream preparing applies to its document");
    assert!(applied["commit"].is_string(), "{applied}");
    assert_eq!(
        sorted_node_names_in(&project.the_graph_document_of("main")),
        ["monitor", "source"]
    );

    std::fs::write(&uv_released, "").expect("uv is released");
    let finished = the_stream_action_once_finished(operations_on_the_loaded_streams, &action).await;
    assert_eq!(finished["state"], "done", "{finished}");
    let listed = tool_answer(
        &tool_call_result(
            Arc::clone(operations_on_the_loaded_streams),
            "list_streams",
            json!({}),
        )
        .await,
    )
    .expect("list_streams answers");
    let listed_main = listed["streams"]
        .as_array()
        .expect("the streams listed")
        .iter()
        .find(|listed_stream| listed_stream["stream"] == main.as_str())
        .unwrap_or_else(|| panic!("the stream is listed: {listed}"));
    // The held uv exits failing, so the stream is loaded failed for its venv, its graph in the
    // stream table.
    assert_eq!(
        listed_main["node_count"], 2,
        "the stream loaded with the edit rather than being held from its document: {listed_main}"
    );
    let read_once_loaded = tool_answer(
        &tool_call_result(
            Arc::clone(operations_on_the_loaded_streams),
            "graph",
            json!({ "stream": main }),
        )
        .await,
    )
    .expect("the loaded stream answers its graph");
    assert_eq!(
        sorted_node_names_in(&read_once_loaded),
        ["monitor", "source"],
        "the load took in the edit made while the project prepared: {read_once_loaded}"
    );
}

/// An action the runtime never queued, and a relative project directory, are refused
/// saying so.
#[tokio::test(flavor = "multi_thread")]
async fn describe_stream_action_and_open_project_refuse_what_the_runtime_cannot_hold() {
    let engine = AnEngineRecordingItsStreamsInATemporaryStateDirectory::new();
    let operations_on_the_loaded_streams = &engine.operations_on_the_loaded_streams();

    let never_queued = tool_answer(
        &tool_call_result(
            Arc::clone(operations_on_the_loaded_streams),
            "describe_stream_action",
            json!({ "action": "Rgone-7" }),
        )
        .await,
    )
    .unwrap_err();
    assert!(
        never_queued.contains("the stream action `Rgone-7` is not held"),
        "{never_queued}"
    );
    let relative = tool_answer(
        &tool_call_result(
            Arc::clone(operations_on_the_loaded_streams),
            "open_project",
            json!({ "project_directory": "desk" }),
        )
        .await,
    )
    .unwrap_err();
    assert!(
        relative.contains("`project_directory` must be an absolute path"),
        "{relative}"
    );
}
