// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

#[test]
#[serial]
fn a_stop_queued_behind_a_removal_of_its_project_is_refused() {
    let state_directory = tempfile::tempdir().unwrap();
    let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
    let project = ProjectWithGraphDocuments::holding(the_graph_of_a_stream_named("camera"));
    let mut removed_with_its_project = a_record_of(&project, "camera");
    removed_with_its_project
        .queue_the_pending_stream_action("R-1", PendingStreamActionKind::Remove);
    records_in(state_directory.path())
        .write(&removed_with_its_project)
        .unwrap();

    let refusal = refusal_of(runner.stop_playback(project.path()));

    assert!(
        refusal.contains("being removed with its project"),
        "{refusal}"
    );
    assert_eq!(
        the_pending_stream_actions_on(state_directory.path(), &removed_with_its_project.stream_id)
            .len(),
        1
    );
}

#[test]
#[serial]
fn a_remove_project_unloads_and_forgets_every_stream_of_the_project_shown_stopping_until_then() {
    let state_directory = tempfile::tempdir().unwrap();
    let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
    let project = ProjectWithGraphDocuments::holding(the_graph_of_a_stream_named("camera"));
    let other_project = ProjectWithGraphDocuments::holding(the_graph_of_a_stream_named("preview"));
    let camera = a_recorded_stream_loaded_without_its_start(
        &runner,
        state_directory.path(),
        &a_record_of(&project, "camera"),
    );
    project.hold_the_streams(&["camera", "mic"], &[]);
    let mut mic = a_record_of(&project, "mic");
    mic.stopped = true;
    records_in(state_directory.path()).write(&mic).unwrap();
    let preview = a_record_of(&other_project, "preview");
    records_in(state_directory.path()).write(&preview).unwrap();
    let (stub_uv, gate) = a_uv_making_the_venv_once_released(&project);
    stub_uv.handed_to(&runner);
    let run_ahead_of_it = runner
        .reload_project(project_open_or_reload_request_of(project.path()))
        .unwrap();
    the_stream_action_once(&runner, &run_ahead_of_it.action_id, |action| {
        action.step == Some(StreamActionStep::MakingTheVenv)
    });

    let removal = runner.remove_project(project.path()).unwrap();

    assert_eq!(
        the_pending_stream_actions_on(state_directory.path(), &mic.stream_id),
        [(removal.action_id.clone(), PendingStreamActionKind::Remove)]
    );
    let mic_listed = runner
        .list_streams()
        .into_iter()
        .find(|listing| listing.stream_id == mic.stream_id)
        .unwrap();
    assert_eq!(mic_listed.state, StreamListingState::Preparing);
    std::fs::write(&gate, "").unwrap();
    the_stream_action_once_finished(&runner, &run_ahead_of_it.action_id);
    let removed = the_stream_action_once_finished(&runner, &removal.action_id);

    assert_eq!(removed.kind, StreamActionKind::Remove);
    assert_eq!(removed.state, StreamActionState::Done, "{removed:?}");
    assert!(camera.has_ended());
    assert!(the_current_record_in(state_directory.path(), camera.stream_id()).is_none());
    assert!(the_current_record_in(state_directory.path(), &mic.stream_id).is_none());
    assert!(the_current_record_in(state_directory.path(), &preview.stream_id).is_some());
    assert!(runner.ids_of_the_loaded_streams().is_empty());
    assert!(
        refusal_of(runner.remove_project(project.path()))
            .contains("holds no stream of the project")
    );
    assert!(refusal_of(runner.remove_project(Path::new("relative"))).contains("absolute"));
}

#[cfg(target_os = "linux")]
#[test]
#[serial]
fn a_delete_project_forgets_every_stream_of_the_project_and_moves_its_folder_to_the_trash() {
    let state_directory = tempfile::tempdir().unwrap();
    let data_home = tempfile::tempdir().unwrap();
    // SAFETY: the test is serial, so no other thread reads the environment meanwhile.
    unsafe { std::env::set_var("XDG_DATA_HOME", data_home.path()) };
    let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
    let project = ProjectWithGraphDocuments::holding(the_graph_of_a_stream_named("camera"));
    std::fs::write(project.path().join(PROJECT_FILE_NAME), "{}").unwrap();
    let camera = a_recorded_stream_loaded_without_its_start(
        &runner,
        state_directory.path(),
        &a_record_of(&project, "camera"),
    );
    let folder_name = project.path().file_name().unwrap().to_owned();

    let deletion = runner.delete_project(project.path()).unwrap();
    let deleted = the_stream_action_once_finished(&runner, &deletion.action_id);
    unsafe { std::env::remove_var("XDG_DATA_HOME") };

    assert_eq!(deleted.kind, StreamActionKind::Delete);
    assert_eq!(deleted.state, StreamActionState::Done, "{deleted:?}");
    assert!(camera.has_ended());
    assert!(the_current_record_in(state_directory.path(), camera.stream_id()).is_none());
    assert!(!project.path().exists());
    let trashed = data_home.path().join("Trash/files").join(&folder_name);
    assert!(trashed.join(PROJECT_FILE_NAME).is_file());
    assert!(refusal_of(runner.delete_project(project.path())).contains("holds no project"));
    assert!(refusal_of(runner.delete_project(Path::new("relative"))).contains("absolute"));
}
