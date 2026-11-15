// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

#[test]
#[serial]
fn an_action_whose_body_panics_is_recorded_failed_and_its_projects_next_action_runs() {
    let runner = a_runner_recording_its_streams_in(None);
    let project = tempfile::tempdir().unwrap();
    let queue_of_each_project = &runner.stream_actions.stream_action_queue_of_each_project;
    let an_action_of_the_project = |action_id: &str| StreamAction {
        action_id: action_id.to_string(),
        project_directory: project.path().to_path_buf(),
        kind: StreamActionKind::Load,
        state: StreamActionState::Queued,
        step: None,
        venv_sync: None,
        streams: Vec::new(),
        failed_because: None,
    };
    let panicking = queue_of_each_project.queue_the_stream_action(
        an_action_of_the_project("test-1"),
        Box::new(|_: &Runner| panic!("a body panicking on purpose")),
        &runner.tokio_handle(),
    );
    let after_it = queue_of_each_project.queue_the_stream_action(
        an_action_of_the_project("test-2"),
        Box::new(|runner: &Runner| {
            runner
                .stream_actions
                .stream_action_queue_of_each_project
                .change_the_held_stream_action("test-2", |action| {
                    action.state = StreamActionState::Done
                });
        }),
        &runner.tokio_handle(),
    );

    let after_it_finished = the_stream_action_once_finished(&runner, &after_it.action_id);
    let panicked = runner.describe_stream_action(&panicking.action_id).unwrap();

    assert_eq!(after_it_finished.state, StreamActionState::Done);
    assert_eq!(panicked.state, StreamActionState::Failed, "{panicked:?}");
    assert_eq!(
        panicked.failed_because.as_deref(),
        Some("the action panicked")
    );
}

#[test]
#[serial]
fn one_projects_actions_run_in_order_while_another_projects_run_beside_them() {
    let state_directory = tempfile::tempdir().unwrap();
    let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
    let waiting_on_its_venv =
        ProjectWithGraphDocuments::holding(the_graph_of_a_stream_named("camera"));
    let (stub_uv, gate) = a_uv_making_the_venv_once_released(&waiting_on_its_venv);
    stub_uv.handed_to(&runner);
    let beside_it = ProjectWithGraphDocuments::holding(the_graph_of_a_stream_named("preview"));

    let first = runner
        .open_project(project_open_or_reload_request_of(
            waiting_on_its_venv.path(),
        ))
        .unwrap();
    let second = runner
        .reload_project(project_open_or_reload_request_of(
            waiting_on_its_venv.path(),
        ))
        .unwrap();
    the_stream_action_once(&runner, &first.action_id, |action| {
        action.step == Some(StreamActionStep::MakingTheVenv)
    });
    let other_project = runner
        .open_project(project_open_or_reload_request_of(beside_it.path()))
        .unwrap();
    let other_project_done = the_stream_action_once_finished(&runner, &other_project.action_id);

    assert_eq!(
        other_project_done.state,
        StreamActionState::Done,
        "{other_project_done:?}"
    );
    assert_eq!(
        runner
            .describe_stream_action(&first.action_id)
            .unwrap()
            .state,
        StreamActionState::InProgress,
        "another project's queue never waits on this one's venv"
    );
    assert_eq!(
        runner
            .describe_stream_action(&second.action_id)
            .unwrap()
            .state,
        StreamActionState::Queued,
        "a project's second action waits for its first"
    );
    assert_ne!(first.action_id, second.action_id);
    let preparing_while_the_first_makes_the_venv: Vec<_> = runner
        .list_streams()
        .into_iter()
        .filter(|listing| listing.state == StreamListingState::Preparing)
        .map(|listing| listing.stream_action_under_way)
        .collect();
    assert_eq!(
        preparing_while_the_first_makes_the_venv,
        [Some(StreamActionWorkingOnAStream {
            action_id: first.action_id.clone(),
            step: Some(StreamActionStep::MakingTheVenv),
        })],
        "the project shows the action running on it, never the one queued behind it"
    );

    std::fs::write(&gate, "").unwrap();
    let second_done = the_stream_action_once_finished(&runner, &second.action_id);

    assert_eq!(
        second_done.state,
        StreamActionState::Done,
        "{second_done:?}"
    );
    assert_eq!(
        runner
            .describe_stream_action(&first.action_id)
            .unwrap()
            .state,
        StreamActionState::Done
    );
    assert_eq!(
        runner.ids_of_the_loaded_streams(),
        [
            waiting_on_its_venv.stream_id_of("camera"),
            beside_it.stream_id_of("preview")
        ]
        .into_iter()
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>()
    );
}

#[test]
#[serial]
fn an_action_never_held_is_refused_naming_it() {
    let runner = a_runner_recording_its_streams_in(None);

    assert!(
        refusal_of(runner.describe_stream_action("Rgone-7"))
            .contains("the stream action `Rgone-7` is not held")
    );
}

#[test]
#[serial]
fn a_stop_then_a_run_of_the_project_each_finish_in_order_behind_the_run_ahead_of_them() {
    let state_directory = tempfile::tempdir().unwrap();
    let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
    let project = ProjectWithGraphDocuments::holding(the_graph_of_a_stream_named("camera"));
    let camera = project.stream_id_of("camera");
    a_recorded_stream_loaded_without_its_start(
        &runner,
        state_directory.path(),
        &a_record_of(&project, "camera"),
    );
    let (stub_uv, gate) = a_uv_making_the_venv_once_released(&project);
    stub_uv.handed_to(&runner);
    let run_ahead_of_them = runner
        .reload_project(project_open_or_reload_request_of(project.path()))
        .unwrap();
    the_stream_action_once(&runner, &run_ahead_of_them.action_id, |action| {
        action.step == Some(StreamActionStep::MakingTheVenv)
    });

    let stop = runner.stop_playback(project.path()).unwrap();
    let run_after_the_stop = runner.start_playback(project.path()).unwrap();

    assert_eq!(
        the_pending_stream_actions_on(state_directory.path(), &camera),
        [(stop.action_id.clone(), PendingStreamActionKind::Stop)]
    );
    assert_eq!(
        runner
            .describe_stream_action(&stop.action_id)
            .unwrap()
            .state,
        StreamActionState::Queued
    );

    std::fs::write(&gate, "").unwrap();
    let stopped = the_stream_action_once_finished(&runner, &stop.action_id);
    let run_again = the_stream_action_once_finished(&runner, &run_after_the_stop.action_id);

    assert_eq!(stopped.kind, StreamActionKind::Stop);
    assert_eq!(stopped.state, StreamActionState::Done, "{stopped:?}");
    assert_eq!(stopped.streams[0].state, Some(StreamListingState::Stopped));
    assert_eq!(run_again.state, StreamActionState::Done, "{run_again:?}");
    assert_eq!(
        run_again.streams[0].state,
        Some(StreamListingState::Running)
    );
    let record = the_record_of(state_directory.path(), &camera);
    assert!(!record.stopped);
    assert!(record.pending_stream_actions.is_empty(), "{record:?}");
    assert_eq!(runner.ids_of_the_loaded_streams(), [camera]);
}

#[test]
#[serial]
fn a_stop_queued_behind_a_run_queued_behind_a_stop_stops_the_project_last() {
    let state_directory = tempfile::tempdir().unwrap();
    let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
    let project = ProjectWithGraphDocuments::holding(the_graph_of_a_stream_named("camera"));
    let camera = project.stream_id_of("camera");
    a_recorded_stream_loaded_without_its_start(
        &runner,
        state_directory.path(),
        &a_record_of(&project, "camera"),
    );
    let (stub_uv, gate) = a_uv_making_the_venv_once_released(&project);
    stub_uv.handed_to(&runner);
    let run_ahead_of_them = runner
        .reload_project(project_open_or_reload_request_of(project.path()))
        .unwrap();
    the_stream_action_once(&runner, &run_ahead_of_them.action_id, |action| {
        action.step == Some(StreamActionStep::MakingTheVenv)
    });

    let first_stop = runner.stop_playback(project.path()).unwrap();
    let run_between_the_stops = runner.start_playback(project.path()).unwrap();
    let last_stop = runner.stop_playback(project.path()).unwrap();

    assert_eq!(
        the_pending_stream_actions_on(state_directory.path(), &camera),
        [
            (first_stop.action_id.clone(), PendingStreamActionKind::Stop),
            (last_stop.action_id.clone(), PendingStreamActionKind::Stop),
        ]
    );

    std::fs::write(&gate, "").unwrap();
    the_stream_action_once_finished(&runner, &first_stop.action_id);
    let run_between = the_stream_action_once_finished(&runner, &run_between_the_stops.action_id);
    let stopped_last = the_stream_action_once_finished(&runner, &last_stop.action_id);

    assert_eq!(
        run_between.state,
        StreamActionState::Done,
        "{run_between:?}"
    );
    assert_eq!(
        stopped_last.state,
        StreamActionState::Done,
        "{stopped_last:?}"
    );
    assert_eq!(
        stopped_last.streams[0].state,
        Some(StreamListingState::Stopped)
    );
    let record = the_record_of(state_directory.path(), &camera);
    assert!(record.stopped);
    assert!(record.pending_stream_actions.is_empty(), "{record:?}");
    assert!(
        runner
            .loaded_stream_of_the_id(&camera)
            .unwrap()
            .is_held_stopped()
    );
}

#[test]
#[serial]
fn a_queued_stop_cancelled_takes_its_mark_off_and_a_load_cancelled_in_its_venv_leaves_none() {
    let state_directory = tempfile::tempdir().unwrap();
    let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
    let project = ProjectWithGraphDocuments::holding(the_graph_of_a_stream_named("camera"));
    let camera = project.stream_id_of("camera");
    a_recorded_stream_loaded_without_its_start(
        &runner,
        state_directory.path(),
        &a_record_of(&project, "camera"),
    );
    let (stub_uv, _gate_never_released) = a_uv_making_the_venv_once_released(&project);
    stub_uv.handed_to(&runner);
    let load = runner
        .reload_project(project_open_or_reload_request_of(project.path()))
        .unwrap();
    the_stream_action_once(&runner, &load.action_id, |action| {
        action.step == Some(StreamActionStep::MakingTheVenv)
    });
    let stop = runner.stop_playback(project.path()).unwrap();

    let stop_cancelled = runner.cancel_stream_action(&stop.action_id).unwrap();
    let load_asked_to_cancel = runner.cancel_stream_action(&load.action_id).unwrap();
    let load_cancelled = the_stream_action_once_finished(&runner, &load.action_id);

    assert_eq!(stop_cancelled.state, StreamActionState::Cancelled);
    assert!(the_pending_stream_actions_on(state_directory.path(), &camera).is_empty());
    assert_eq!(load_asked_to_cancel.state, StreamActionState::InProgress);
    assert_eq!(
        load_cancelled.state,
        StreamActionState::Cancelled,
        "{load_cancelled:?}"
    );
    assert_eq!(load_cancelled.failed_because, None);
    assert!(!project.path().join(".venv").exists());
    assert_eq!(
        runner.ids_of_the_loaded_streams(),
        std::slice::from_ref(&camera)
    );
    assert!(!the_record_of(state_directory.path(), &camera).stopped);
    assert!(
        refusal_of(runner.cancel_stream_action(&load.action_id)).contains("is already cancelled")
    );
    assert!(refusal_of(runner.cancel_stream_action("R-never")).contains("is not held"));
    let follow_on = runner.stop_playback(project.path()).unwrap();
    assert_eq!(
        the_stream_action_once_finished(&runner, &follow_on.action_id).state,
        StreamActionState::Done
    );
}

#[test]
fn a_running_stop_or_removal_refuses_its_cancel_and_a_running_load_takes_it() {
    let running_of = |kind: StreamActionKind| StreamAction {
        action_id: "R-1".to_string(),
        project_directory: PathBuf::from("/srv/desk"),
        kind,
        state: StreamActionState::InProgress,
        step: None,
        venv_sync: None,
        streams: Vec::new(),
        failed_because: None,
    };

    for unload in [StreamActionKind::Stop, StreamActionKind::Remove] {
        let refusal = refusal_of(refuse_the_cancel_of_a_running_unload(&running_of(unload)));
        assert!(refusal.contains("cannot be cut short"), "{refusal}");
    }
    for load in [StreamActionKind::Load, StreamActionKind::ReloadAtStart] {
        assert!(refuse_the_cancel_of_a_running_unload(&running_of(load)).is_ok());
    }
}

#[test]
#[serial]
fn a_fresh_runtime_finishes_the_marks_a_crash_left_in_order_before_it_queues_its_reloads() {
    let state_directory = tempfile::tempdir().unwrap();
    let project = ProjectWithGraphDocuments::holding(the_graph_of_a_stream_named("camera"));
    let records = records_in(state_directory.path());
    project.hold_the_streams(&["camera", "mic"], &[]);
    let running = a_record_of(&project, "camera");
    let mut stopped = a_record_of(&project, "mic");
    stopped.queue_the_pending_stream_action("R-3", PendingStreamActionKind::Stop);
    let mut removed = a_record_of(&project, "preview");
    removed.queue_the_pending_stream_action("R-4", PendingStreamActionKind::Stop);
    removed.queue_the_pending_stream_action("R-5", PendingStreamActionKind::Remove);
    for record in [&running, &stopped, &removed] {
        records.write(record).unwrap();
    }
    let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));

    runner.finish_the_pending_stream_actions_a_crash_left();
    let reloads = runner.queue_the_reload_of_every_project_at_the_start();

    assert_eq!(reloads.len(), 1, "one re-load per project: {reloads:?}");
    assert!(the_current_record_in(state_directory.path(), &removed.stream_id).is_none());
    let mic = the_record_of(state_directory.path(), &stopped.stream_id);
    assert!(
        mic.stopped && mic.pending_stream_actions.is_empty(),
        "{mic:?}"
    );
    let reload = the_stream_action_once_finished(&runner, &reloads[0].action_id);
    assert_eq!(reload.kind, StreamActionKind::ReloadAtStart);
    assert_eq!(reload.state, StreamActionState::Done, "{reload:?}");
    let mut reloaded_states = reload
        .streams
        .iter()
        .map(|stream| (stream.stream_id.clone(), stream.state))
        .collect::<Vec<_>>();
    reloaded_states.sort_by(|left, right| left.0.cmp(&right.0));
    let mut expected_states = vec![
        (running.stream_id.clone(), Some(StreamListingState::Running)),
        (stopped.stream_id.clone(), Some(StreamListingState::Stopped)),
    ];
    expected_states.sort_by(|left, right| left.0.cmp(&right.0));
    assert_eq!(reloaded_states, expected_states);
    assert!(
        the_record_of(state_directory.path(), &running.stream_id)
            .pending_stream_actions
            .is_empty()
    );
    let mut loaded_ids = runner.ids_of_the_loaded_streams();
    loaded_ids.sort();
    let mut expected_loaded_ids = vec![running.stream_id, stopped.stream_id.clone()];
    expected_loaded_ids.sort();
    assert_eq!(loaded_ids, expected_loaded_ids);
    assert!(
        runner
            .loaded_stream_of_the_id(&stopped.stream_id)
            .unwrap()
            .is_held_stopped()
    );
}

#[test]
#[serial]
fn a_stop_queued_behind_a_load_shows_its_stream_stopping_once_the_load_has_run() {
    let state_directory = tempfile::tempdir().unwrap();
    let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
    let project = ProjectWithGraphDocuments::holding(the_graph_of_a_stream_named("camera"));
    let camera = project.stream_id_of("camera");
    a_recorded_stream_loaded_without_its_start(
        &runner,
        state_directory.path(),
        &a_record_of(&project, "camera"),
    );
    let stop = runner.stop_playback(project.path()).unwrap();
    let stopped = the_stream_action_once_finished(&runner, &stop.action_id);

    assert_eq!(stopped.state, StreamActionState::Done);
    assert_eq!(runner.list_streams()[0].state, StreamListingState::Stopped);
    assert_eq!(runner.list_streams()[0].stream_id, camera);
}
