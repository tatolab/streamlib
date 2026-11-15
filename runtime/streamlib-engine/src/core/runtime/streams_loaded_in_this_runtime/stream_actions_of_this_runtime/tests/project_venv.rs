// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

#[test]
#[serial]
fn a_run_in_a_project_with_no_venv_makes_it_with_the_runtime_unit_uv_then_compiles() {
    let state_directory = tempfile::tempdir().unwrap();
    let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
    let project = ProjectWithGraphDocuments::holding(the_graph_of_a_stream_named("camera"));
    let stub_uv = StubRuntimeUnitUv::making_the_venv_moved_aside_from(&project);
    stub_uv.handed_to(&runner);

    let run = runner
        .load_the_project_off_its_queue(project.run_request())
        .unwrap();

    the_one_running_stream_of(&run);
    assert_eq!(stub_uv.runs(), ["sync"]);
    assert!(project.interpreter().is_file());
}

#[test]
#[serial]
fn making_a_projects_venv_current_syncs_it_with_the_runtime_unit_uv_and_loads_no_stream() {
    let state_directory = tempfile::tempdir().unwrap();
    let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
    let project = ProjectWithGraphDocuments::holding(the_graph_of_a_stream_named("camera"));
    let stub_uv = StubRuntimeUnitUv::making_the_venv_moved_aside_from(&project);
    stub_uv.handed_to(&runner);

    let queued = runner
        .make_the_projects_venv_current(project.path())
        .unwrap();

    let done = the_stream_action_once_finished(&runner, &queued.action_id);
    assert_eq!(done.state, StreamActionState::Done, "{done:?}");
    assert_eq!(done.kind, StreamActionKind::MakeTheVenvCurrent);
    assert!(done.streams.is_empty(), "{:?}", done.streams);
    assert_eq!(stub_uv.runs(), ["sync"]);
    assert!(project.interpreter().is_file());
    assert!(runner.list_streams().is_empty());
}

#[test]
#[serial]
fn a_projects_streams_keep_their_listed_state_while_its_venv_is_made_current() {
    let state_directory = tempfile::tempdir().unwrap();
    let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
    let project = ProjectWithGraphDocuments::holding(the_graph_of_a_stream_named("camera"));
    let (stub_uv, gate) = a_uv_making_the_venv_once_released(&project);
    stub_uv.handed_to(&runner);
    std::fs::write(&gate, "").unwrap();
    let opened = runner
        .open_project(project_open_or_reload_request_of(project.path()))
        .unwrap();
    the_stream_action_once_finished(&runner, &opened.action_id);
    let the_listed_states = |runner: &Runner| {
        runner
            .list_streams()
            .into_iter()
            .map(|listing| {
                (
                    listing.stream_id,
                    listing.state,
                    listing.failed_because,
                    listing.stream_action_under_way,
                )
            })
            .collect::<Vec<_>>()
    };
    let listed_before_the_sync = the_listed_states(&runner);
    assert!(!listed_before_the_sync.is_empty());
    std::fs::remove_file(&gate).unwrap();
    std::fs::write(
        project.path().join("pyproject.toml"),
        "[project]\nname = \"camera\"\ndependencies = [\"pyserial\"]\n",
    )
    .unwrap();

    let queued = runner
        .make_the_projects_venv_current(project.path())
        .unwrap();
    the_stream_action_once(&runner, &queued.action_id, |action| {
        action.step == Some(StreamActionStep::MakingTheVenv)
    });

    assert_eq!(the_listed_states(&runner), listed_before_the_sync);
    std::fs::write(&gate, "").unwrap();
    let done = the_stream_action_once_finished(&runner, &queued.action_id);
    assert_eq!(done.state, StreamActionState::Done, "{done:?}");
    assert_eq!(stub_uv.runs().len(), 2, "{:?}", stub_uv.runs());
}

#[test]
#[serial]
fn making_a_projects_venv_current_fails_in_uvs_words_when_its_sync_fails() {
    let state_directory = tempfile::tempdir().unwrap();
    let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
    let project = ProjectWithGraphDocuments::with_no_venv();
    let stub_uv = StubRuntimeUnitUv::failing_writing(UV_RESOLUTION_FAILURE);
    stub_uv.handed_to(&runner);

    let queued = runner
        .make_the_projects_venv_current(project.path())
        .unwrap();

    let failed = the_stream_action_once_finished(&runner, &queued.action_id);
    assert_eq!(failed.state, StreamActionState::Failed, "{failed:?}");
    assert!(
        failed
            .failed_because
            .as_deref()
            .is_some_and(|why| why.contains(UV_RESOLUTION_FAILURE)),
        "{failed:?}"
    );
}

#[test]
fn making_the_venv_current_of_a_project_not_named_by_its_absolute_path_is_refused() {
    let runner = a_runner_recording_its_streams_in(None);

    let refused = runner
        .make_the_projects_venv_current(Path::new("relative/project"))
        .unwrap_err();

    assert!(refused.to_string().contains("absolute"), "{refused}");
}

#[test]
#[serial]
fn a_run_whose_venv_interpreter_runs_never_runs_uv() {
    let state_directory = tempfile::tempdir().unwrap();
    let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
    let project = ProjectWithGraphDocuments::holding(the_graph_of_a_stream_named("camera"));
    let stub_uv = StubRuntimeUnitUv::failing_writing(UV_RESOLUTION_FAILURE);
    stub_uv.handed_to(&runner);

    let run = runner
        .load_the_project_off_its_queue(project.run_request())
        .unwrap();

    the_one_running_stream_of(&run);
    assert!(stub_uv.runs().is_empty(), "{:?}", stub_uv.runs());
}

#[test]
#[serial]
fn a_run_whose_venv_interpreter_does_not_run_has_its_venv_made_again() {
    let state_directory = tempfile::tempdir().unwrap();
    let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
    let project = ProjectWithGraphDocuments::holding(the_graph_of_a_stream_named("camera"));
    let stub_uv = StubRuntimeUnitUv::making_the_venv_moved_aside_from(&project);
    stub_uv.handed_to(&runner);
    directory_at_an_explicit_mode::create_directory_and_its_missing_parents_at_mode(
        &project.path().join(".venv").join("bin"),
        0o755,
    )
    .unwrap();
    std::os::unix::fs::symlink("/nonexistent/python3.12", project.interpreter()).unwrap();

    let run = runner
        .load_the_project_off_its_queue(project.run_request())
        .unwrap();

    the_one_running_stream_of(&run);
    assert_eq!(stub_uv.runs(), ["sync"]);
}

#[test]
#[serial]
fn a_run_whose_venv_uv_cannot_make_holds_each_stream_stopped_from_its_document_failed_in_uvs_words()
{
    let state_directory = tempfile::tempdir().unwrap();
    let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
    let project = ProjectWithGraphDocuments::with_no_venv();
    project.write_the_graph_document(the_graph_of_a_stream_named("camera"));
    let stub_uv = StubRuntimeUnitUv::failing_writing(UV_RESOLUTION_FAILURE);
    stub_uv.handed_to(&runner);

    let run = runner
        .load_the_project_off_its_queue(project.run_request())
        .unwrap();

    let (stream, failed_because) = the_one_failed_stream_of(&run);
    assert!(
        failed_because.contains(UV_RESOLUTION_FAILURE),
        "{failed_because}"
    );
    assert_eq!(stream.stream_location, project.location_of("camera"));
    assert!(
        !project.path().join(".venv").exists(),
        "the half-made venv was left"
    );
    let held = runner.loaded_stream_of_the_id(&stream.stream_id).unwrap();
    assert!(held.is_held_stopped() && held.runtime_context.lock().is_none());
    let listed = runner
        .list_streams()
        .into_iter()
        .find(|listed| listed.stream_id == stream.stream_id)
        .unwrap();
    assert_eq!(listed.state, StreamListingState::Failed);
    assert_eq!(listed.failed_because.as_deref(), Some(failed_because));
    assert_eq!(
        the_record_of(state_directory.path(), &stream.stream_id).failed_because,
        Some(failed_because.to_string())
    );
}

#[test]
#[serial]
fn a_run_whose_venv_uv_cannot_make_writes_uvs_lines_to_the_projects_log_under_no_stream() {
    let state_directory = tempfile::tempdir().unwrap();
    let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
    let project = ProjectWithGraphDocuments::with_no_venv();
    let stub_uv = StubRuntimeUnitUv::failing_writing(UV_RESOLUTION_FAILURE);
    stub_uv.handed_to(&runner);

    runner
        .load_the_project_off_its_queue(project.run_request())
        .unwrap();

    let page = streamlib_runtime_client_contract::project_log_records::project_log_records_after(
        &std::fs::canonicalize(project.path()).unwrap(),
        None,
        None,
        None,
        4096,
    )
    .expect("the project's log reads");
    let uv_line = page
        .records
        .iter()
        .find(|record| {
            record["message"]
                .as_str()
                .is_some_and(|message| message.contains(UV_RESOLUTION_FAILURE))
        })
        .unwrap_or_else(|| panic!("the project's log lacks uv's line: {:#?}", page.records));
    assert!(
        uv_line.get("stream").is_none_or(serde_json::Value::is_null),
        "{uv_line}"
    );
    assert_eq!(
        uv_line["runtime_id"].as_str(),
        Some(runner.runtime_id().to_string().as_str())
    );
}

#[test]
#[serial]
fn a_run_whose_venv_uv_cannot_make_unloads_and_fails_every_record_of_the_project() {
    let state_directory = tempfile::tempdir().unwrap();
    let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
    let project = ProjectWithGraphDocuments::holding(the_graph_of_a_stream_named("camera"));
    let camera = project.stream_id_of("camera");
    runner
        .load_the_project_off_its_queue(project.run_request())
        .unwrap();
    let running = runner.loaded_stream_of_the_id(&camera).unwrap();
    std::fs::remove_dir_all(project.path().join(".venv")).unwrap();
    let stub_uv = StubRuntimeUnitUv::failing_writing(UV_RESOLUTION_FAILURE);
    stub_uv.handed_to(&runner);

    let run = runner
        .load_the_project_off_its_queue(project.run_request())
        .unwrap();

    let (stream, failed_because) = the_one_failed_stream_of(&run);
    assert_eq!(stream.stream_id, camera);
    assert!(
        failed_because.contains(UV_RESOLUTION_FAILURE),
        "{failed_because}"
    );
    assert!(running.has_ended());
    assert!(the_record_of(state_directory.path(), &camera).is_failed());
}

#[test]
#[serial]
fn a_run_on_a_runtime_given_no_uv_holds_each_stream_stopped_failed_saying_the_unit_carries_none() {
    let state_directory = tempfile::tempdir().unwrap();
    let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
    let project = ProjectWithGraphDocuments::with_no_venv();
    project.write_the_graph_document(the_graph_of_a_stream_named("camera"));

    let run = runner
        .load_the_project_off_its_queue(project.run_request())
        .unwrap();

    let (stream, failed_because) = the_one_failed_stream_of(&run);
    assert!(
        failed_because.contains(
            "the runtime unit carries no uv; rebuild it with `cargo xtask build-runtime`"
        ),
        "{failed_because}"
    );
    assert_eq!(stream.stream_location, project.location_of("camera"));
    assert!(
        runner
            .loaded_stream_of_the_id(&stream.stream_id)
            .unwrap()
            .is_held_stopped()
    );
}

#[test]
fn a_runtime_handed_its_uv_twice_refuses_the_second() {
    let runner = a_runner_recording_its_streams_in(None);
    runner
        .set_runtime_unit_uv_executable(PathBuf::from("/opt/tatolab/bin/uv"))
        .unwrap();

    let refusal = refusal_of(runner.set_runtime_unit_uv_executable(PathBuf::from("/elsewhere/uv")));

    assert!(refusal.contains("/opt/tatolab/bin/uv"), "{refusal}");
}
