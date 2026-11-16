// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

#[test]
#[serial]
fn a_stop_of_a_stopped_project_is_refused_with_nothing_queued() {
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
    let refusal = refusal_of(runner.stop_playback(project.path()));
    assert!(refusal.contains("none to stop"), "{refusal}");
    let elsewhere = ProjectWithGraphDocuments::with_no_venv();
    assert!(refusal_of(runner.stop_playback(elsewhere.path())).contains("none to stop"));
    assert!(refusal_of(runner.stop_playback(Path::new("relative"))).contains("absolute"));

    the_stream_action_once_finished(&runner, &stop.action_id);
    assert!(the_pending_stream_actions_on(state_directory.path(), &camera).is_empty());
    assert!(the_record_of(state_directory.path(), &camera).stopped);
}

#[test]
#[serial]
fn a_stop_of_a_project_stops_each_running_stream_and_leaves_a_failed_one_as_it_is() {
    let state_directory = tempfile::tempdir().unwrap();
    let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
    let project = ProjectWithGraphDocuments::holding(the_graph_of_a_stream_named("camera"));
    project.write_the_graph_document(the_graph_of_a_stream_named("microphone"));
    let camera = project.stream_id_of("camera");
    let microphone = project.stream_id_of("microphone");
    a_recorded_stream_loaded_without_its_start(
        &runner,
        state_directory.path(),
        &a_record_of(&project, "camera"),
    );
    a_recorded_stream_loaded_without_its_start(
        &runner,
        state_directory.path(),
        &a_record_of(&project, "microphone"),
    );
    let failed = StreamRecord {
        failed_because: Some("it crashed the runtime".to_string()),
        ..a_record_of(&project, "preview")
    };
    records_in(state_directory.path()).write(&failed).unwrap();

    let stop = runner.stop_playback(project.path()).unwrap();
    let stopped = the_stream_action_once_finished(&runner, &stop.action_id);

    assert_eq!(stopped.state, StreamActionState::Done, "{stopped:?}");
    let mut stream_states: Vec<_> = stopped
        .streams
        .iter()
        .map(|stream| (stream.stream_id.clone(), stream.state))
        .collect();
    stream_states.sort_by(|first, second| first.0.cmp(&second.0));
    let mut expected = vec![
        (camera.clone(), Some(StreamListingState::Stopped)),
        (microphone.clone(), Some(StreamListingState::Stopped)),
    ];
    expected.sort_by(|first, second| first.0.cmp(&second.0));
    assert_eq!(stream_states, expected);
    let mut held_ids = runner.ids_of_the_loaded_streams();
    held_ids.sort();
    let mut expected_held_ids = vec![camera.clone(), microphone.clone()];
    expected_held_ids.sort();
    assert_eq!(held_ids, expected_held_ids);
    for held_id in &held_ids {
        assert!(
            runner
                .loaded_stream_of_the_id(held_id)
                .unwrap()
                .is_held_stopped()
        );
    }
    assert!(the_record_of(state_directory.path(), &camera).stopped);
    assert!(the_record_of(state_directory.path(), &microphone).stopped);
    let preview = the_record_of(state_directory.path(), &failed.stream_id);
    assert!(preview.is_failed() && !preview.stopped, "{preview:?}");
    assert!(preview.pending_stream_actions.is_empty(), "{preview:?}");
}

/// The graph of a stream named `stream_name`: an output-only mock's `out1` linked into
/// an input-only mock's `in1`.
fn the_graph_of_a_stream_of_a_source_linked_into_a_sink(stream_name: &str) -> serde_json::Value {
    serde_json::json!({
        "stream": stream_name,
        "nodes": [
            {
                "name": "source",
                "type": MockOutputOnlyNode::Node::node_class_import_path().as_str(),
                "config": {},
            },
            {
                "name": "sink",
                "type": MockInputOnlyNode::Node::node_class_import_path().as_str(),
                "config": {},
            },
        ],
        "links": [{
            "source": { "node": "source", "port": "out1" },
            "target": { "node": "sink", "port": "in1" },
        }],
    })
}

#[test]
#[serial]
fn a_stopped_project_holds_its_graph_to_edit_and_plays_it_fresh_with_no_load() {
    let state_directory = tempfile::tempdir().unwrap();
    let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
    let project = ProjectWithGraphDocuments::holding(
        the_graph_of_a_stream_of_a_source_linked_into_a_sink("camera"),
    );
    let camera = project.stream_id_of("camera");
    runner
        .load_the_project_off_its_queue(project.run_request())
        .unwrap();
    let playing = runner.loaded_stream_of_the_id(&camera).unwrap();
    assert_eq!(playing.status(), RuntimeStatus::Started);

    let stop = runner.stop_playback(project.path()).unwrap();
    let stopped = the_stream_action_once_finished(&runner, &stop.action_id);

    assert_eq!(stopped.state, StreamActionState::Done, "{stopped:?}");
    let held = runner
        .loaded_stream_of_the_id(&camera)
        .expect("the stream stays held");
    assert!(
        Arc::ptr_eq(&held, &playing),
        "the stop loads no other stream"
    );
    assert!(held.is_held_stopped());
    assert!(held.runtime_context.lock().is_none());
    assert_eq!(held.node_count(), 2);
    assert_eq!(held.the_live_links_by_node_uuid().len(), 1);
    let listing = runner
        .list_streams()
        .into_iter()
        .find(|listed| listed.stream_id == camera)
        .unwrap();
    assert_eq!(listing.state, StreamListingState::Stopped);
    assert_eq!(listing.node_count, Some(2));
    assert!(the_record_of(state_directory.path(), &camera).stopped);
    let pause_refusal = refusal_of(runner.pause_playback(project.path()));
    assert!(pause_refusal.contains("none to pause"), "{pause_refusal}");

    held.add_node(
        crate::core::nodes::NodeSpec::new(
            MockOutputOnlyNode::Node::node_class_import_path(),
            serde_json::json!({}),
        )
        .with_display_name("second-source".to_string()),
    )
    .expect("a stopped stream takes an edit");
    project.remove_the_graph_document("camera");

    let play = runner.start_playback(project.path()).unwrap();
    let played = the_stream_action_once_finished(&runner, &play.action_id);

    assert_eq!(played.state, StreamActionState::Done, "{played:?}");
    assert_eq!(played.streams[0].state, Some(StreamListingState::Running));
    let playing_again = runner.loaded_stream_of_the_id(&camera).unwrap();
    assert!(
        Arc::ptr_eq(&playing_again, &playing),
        "the play loads no other stream"
    );
    assert_eq!(playing_again.stream_tag(), playing.stream_tag());
    assert!(!playing_again.is_held_stopped());
    assert_eq!(playing_again.status(), RuntimeStatus::Started);
    assert_eq!(playing_again.node_count(), 3);
    assert!(!the_record_of(state_directory.path(), &camera).stopped);
}

#[test]
#[serial]
fn a_link_and_a_node_removed_from_a_stopped_stream_leave_its_loaded_graph_at_once() {
    let state_directory = tempfile::tempdir().unwrap();
    let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
    let project = ProjectWithGraphDocuments::holding(
        the_graph_of_a_stream_of_a_source_linked_into_a_sink("camera"),
    );
    let camera = project.stream_id_of("camera");
    runner
        .load_the_project_off_its_queue(project.run_request())
        .unwrap();
    let stop = runner.stop_playback(project.path()).unwrap();
    the_stream_action_once_finished(&runner, &stop.action_id);
    let held = runner.loaded_stream_of_the_id(&camera).unwrap();
    assert!(held.is_held_stopped() && held.runtime_context.lock().is_none());
    let graph_as_held =
        streamlib_project_repository::StreamGraphDocument::from_value(held.to_json().unwrap())
            .unwrap();
    let link = graph_as_held.links().remove(0);
    let sink = graph_as_held.node_named("sink").unwrap();
    let tokio_runtime_of_the_test = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let apply = |graph_operation| {
        tokio_runtime_of_the_test.block_on(
            <LoadedStreamInThisRuntime as crate::core::runtime::operations::RuntimeOperations>::apply_a_stream_graph_operation_live(
                &held,
                graph_operation,
            ),
        )
    };

    apply(streamlib_project_repository::StreamGraphOperation::Disconnect { link })
        .expect("a stopped stream takes a disconnect");
    assert!(held.the_live_links_by_node_uuid().is_empty());
    apply(
        streamlib_project_repository::StreamGraphOperation::RemoveNode {
            node: sink.clone(),
            links: Vec::new(),
        },
    )
    .expect("a stopped stream takes a removal");

    assert_eq!(held.node_count(), 1);
    let graph_after =
        streamlib_project_repository::StreamGraphDocument::from_value(held.to_json().unwrap())
            .unwrap();
    assert!(graph_after.node_named("sink").is_none(), "{graph_after:?}");
    assert!(graph_after.links().is_empty());
    let listing = runner
        .list_streams()
        .into_iter()
        .find(|listed| listed.stream_id == camera)
        .unwrap();
    assert_eq!(listing.node_count, Some(1));
    let removed_again = apply(
        streamlib_project_repository::StreamGraphOperation::RemoveNode {
            node: sink,
            links: Vec::new(),
        },
    );
    assert!(
        removed_again.is_err(),
        "the node is gone from the loaded graph"
    );

    let play = runner.start_playback(project.path()).unwrap();
    let played = the_stream_action_once_finished(&runner, &play.action_id);
    assert_eq!(played.state, StreamActionState::Done, "{played:?}");
    assert_eq!(held.status(), RuntimeStatus::Started);
    assert_eq!(held.node_count(), 1);
}

#[test]
#[serial]
fn a_stop_after_a_play_tears_every_instance_down_again_and_keeps_the_graph() {
    let state_directory = tempfile::tempdir().unwrap();
    let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
    let project = ProjectWithGraphDocuments::holding(
        the_graph_of_a_stream_of_a_source_linked_into_a_sink("camera"),
    );
    let camera = project.stream_id_of("camera");
    runner
        .load_the_project_off_its_queue(project.run_request())
        .unwrap();
    for _ in 0..2 {
        let stop = runner.stop_playback(project.path()).unwrap();
        the_stream_action_once_finished(&runner, &stop.action_id);
        let play = runner.start_playback(project.path()).unwrap();
        the_stream_action_once_finished(&runner, &play.action_id);
    }
    let stop = runner.stop_playback(project.path()).unwrap();
    the_stream_action_once_finished(&runner, &stop.action_id);

    let held = runner.loaded_stream_of_the_id(&camera).unwrap();
    assert!(held.is_held_stopped());
    assert_eq!(held.node_count(), 2);
    assert_eq!(held.the_live_links_by_node_uuid().len(), 1);
    let instances_left = held.compiler.scope_reading(|graph| {
        graph
            .traversal()
            .v(())
            .iter()
            .filter(|node| {
                crate::core::graph::GraphNodeWithComponents::has::<
                    crate::core::graph::NodeInstanceComponent,
                >(*node)
            })
            .count()
    });
    assert_eq!(instances_left, 0, "a stopped graph holds no node instance");
}

#[test]
#[serial]
fn a_stream_that_fails_before_the_stop_reaches_it_is_left_failed_with_its_mark_taken_off() {
    let state_directory = tempfile::tempdir().unwrap();
    let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
    let project = ProjectWithGraphDocuments::holding(the_graph_of_a_stream_named("camera"));
    let camera = project.stream_id_of("camera");
    records_in(state_directory.path())
        .write(&a_record_of(&project, "camera"))
        .unwrap();
    project.write_an_unloadable_graph_document("camera");
    let (stub_uv, gate) = a_uv_making_the_venv_once_released(&project);
    stub_uv.handed_to(&runner);
    let run_ahead_of_it = runner
        .reload_project(project_open_or_reload_request_of(project.path()))
        .unwrap();
    the_stream_action_once(&runner, &run_ahead_of_it.action_id, |action| {
        action.step == Some(StreamActionStep::MakingTheVenv)
    });
    let stop = runner.stop_playback(project.path()).unwrap();

    std::fs::write(&gate, "").unwrap();
    the_stream_action_once_finished(&runner, &run_ahead_of_it.action_id);
    let stop_once_finished = the_stream_action_once_finished(&runner, &stop.action_id);

    assert_eq!(
        stop_once_finished.state,
        StreamActionState::Done,
        "{stop_once_finished:?}"
    );
    let record = the_record_of(state_directory.path(), &camera);
    assert!(record.pending_stream_actions.is_empty(), "{record:?}");
    assert!(record.is_failed() && !record.stopped, "{record:?}");
}

fn the_listed_state_of(runner: &Runner, stream_id: &str) -> StreamListingState {
    runner
        .list_streams()
        .into_iter()
        .find(|listing| listing.stream_id == stream_id)
        .unwrap_or_else(|| panic!("the stream `{stream_id}` is listed"))
        .state
}

#[test]
#[serial]
fn a_pause_or_resume_of_a_relative_directory_or_a_project_running_nothing_is_refused() {
    let runner = a_runner_recording_its_streams_in(None);
    let idle = ProjectWithGraphDocuments::with_no_venv();

    assert!(refusal_of(runner.pause_playback(Path::new("relative"))).contains("absolute"));
    assert!(refusal_of(runner.resume_playback(Path::new("relative"))).contains("absolute"));
    let pause_refusal = refusal_of(runner.pause_playback(idle.path()));
    assert!(pause_refusal.contains("none to pause"), "{pause_refusal}");
    let resume_refusal = refusal_of(runner.resume_playback(idle.path()));
    assert!(resume_refusal.contains("is not paused"), "{resume_refusal}");
}

#[test]
#[serial]
fn a_lifecycle_or_playback_verb_on_a_project_in_another_state_is_refused_naming_the_verb_that_fits()
{
    let state_directory = tempfile::tempdir().unwrap();
    let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
    let never_opened = ProjectWithGraphDocuments::with_no_venv();
    let project = ProjectWithGraphDocuments::holding(the_graph_of_a_stream_named("camera"));
    a_recorded_stream_loaded_without_its_start(
        &runner,
        state_directory.path(),
        &a_record_of(&project, "camera"),
    );

    for relative_refusal in [
        refusal_of(runner.open_project(project_open_or_reload_request_of(Path::new("relative")))),
        refusal_of(runner.reload_project(project_open_or_reload_request_of(Path::new("relative")))),
        refusal_of(runner.start_playback(Path::new("relative"))),
        refusal_of(runner.close_project(Path::new("relative"))),
    ] {
        assert!(relative_refusal.contains("absolute"), "{relative_refusal}");
    }
    for not_open_refusal in [
        refusal_of(runner.reload_project(project_open_or_reload_request_of(never_opened.path()))),
        refusal_of(runner.start_playback(never_opened.path())),
    ] {
        assert!(
            not_open_refusal.contains("is not open") && not_open_refusal.contains("`open_project`"),
            "{not_open_refusal}"
        );
    }
    let refusal = refusal_of(runner.close_project(never_opened.path()));
    assert!(refusal.contains("is not open"), "{refusal}");
    let refusal =
        refusal_of(runner.open_project(project_open_or_reload_request_of(project.path())));
    assert!(
        refusal.contains("already open") && refusal.contains("`reload_project`"),
        "{refusal}"
    );
    let refusal = refusal_of(runner.start_playback(project.path()));
    assert!(
        refusal.contains("already plays") && refusal.contains("`stop_playback`"),
        "{refusal}"
    );

    runner.pause_playback(project.path()).unwrap();
    let refusal = refusal_of(runner.start_playback(project.path()));
    assert!(
        refusal.contains("is paused") && refusal.contains("`resume_playback`"),
        "{refusal}"
    );

    let stop = runner.stop_playback(project.path()).unwrap();
    the_stream_action_once_finished(&runner, &stop.action_id);
    let reload = runner
        .reload_project(project_open_or_reload_request_of(project.path()))
        .expect("a stopped project is still open, so it reloads");
    let reloaded = the_stream_action_once_finished(&runner, &reload.action_id);
    assert_eq!(
        reloaded.streams[0].state,
        Some(StreamListingState::Stopped),
        "a reload keeps the project's playback stopped: {reloaded:?}"
    );
}

#[test]
#[serial]
fn a_paused_project_stays_paused_until_its_resume_or_its_stop_and_is_recorded_paused() {
    let state_directory = tempfile::tempdir().unwrap();
    let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
    let project = ProjectWithGraphDocuments::holding(the_graph_of_a_stream_named("camera"));
    let camera = project.stream_id_of("camera");
    a_recorded_stream_loaded_without_its_start(
        &runner,
        state_directory.path(),
        &a_record_of(&project, "camera"),
    );

    let paused = runner.pause_playback(project.path()).unwrap();
    assert_eq!(
        paused.stream_ids,
        Vec::<String>::new(),
        "an unstarted stream is not paused"
    );
    let paused_again = runner.pause_playback(project.path()).unwrap();
    assert_eq!(paused_again.project_directory, paused.project_directory);
    let recorded_paused = the_record_of(state_directory.path(), &camera);
    assert!(!recorded_paused.stopped);
    assert!(recorded_paused.paused);
    assert!(recorded_paused.comes_back_paused_at_a_restart());
    assert!(runner.resume_playback(project.path()).is_ok());
    assert!(!the_record_of(state_directory.path(), &camera).paused);
    let refusal = refusal_of(runner.resume_playback(project.path()));
    assert!(refusal.contains("is not paused"), "{refusal}");

    runner.pause_playback(project.path()).unwrap();
    let stop = runner.stop_playback(project.path()).unwrap();
    let refusal = refusal_of(runner.resume_playback(project.path()));
    assert!(refusal.contains("is not paused"), "{refusal}");
    the_stream_action_once_finished(&runner, &stop.action_id);
    let recorded_stopped = the_record_of(state_directory.path(), &camera);
    assert!(recorded_stopped.stopped);
    assert!(!recorded_stopped.paused);
}

#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
#[serial]
fn a_paused_project_lists_paused_re_loads_paused_and_resumes_running_and_a_restart_brings_it_back_paused()
 {
    let state_directory = tempfile::tempdir().unwrap();
    let project = ProjectWithGraphDocuments::holding(the_graph_of_a_stream_named("camera"));
    project.write_the_graph_document(the_graph_of_a_stream_named("microphone"));
    let camera = project.stream_id_of("camera");
    let microphone = project.stream_id_of("microphone");
    let mut both_streams = vec![camera.clone(), microphone.clone()];
    both_streams.sort();
    {
        let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
        runner
            .load_the_project_off_its_queue(project.run_request())
            .unwrap();

        let paused = runner.pause_playback(project.path()).unwrap();
        let mut paused_ids = paused.stream_ids.clone();
        paused_ids.sort();
        assert_eq!(paused_ids, both_streams);
        for stream_id in &both_streams {
            assert_eq!(
                the_listed_state_of(&runner, stream_id),
                StreamListingState::Paused
            );
            assert_eq!(
                runner.loaded_stream_of_the_id(stream_id).unwrap().status(),
                RuntimeStatus::Paused
            );
        }

        runner
            .load_the_project_off_its_queue(project.run_request())
            .expect("a run of the paused project re-loads it");
        for stream_id in &both_streams {
            assert_eq!(
                runner.loaded_stream_of_the_id(stream_id).unwrap().status(),
                RuntimeStatus::Paused,
                "a stream a run re-loads while its project is paused starts paused"
            );
        }

        let resumed = runner.resume_playback(project.path()).unwrap();
        let mut resumed_ids = resumed.stream_ids.clone();
        resumed_ids.sort();
        assert_eq!(resumed_ids, both_streams);
        for stream_id in &both_streams {
            assert_eq!(
                the_listed_state_of(&runner, stream_id),
                StreamListingState::Running
            );
        }

        runner.pause_playback(project.path()).unwrap();
    }

    let restarted = a_runner_recording_its_streams_in(Some(state_directory.path()));
    restarted.reload_every_recorded_stream_not_closed_at_once();
    for stream_id in &both_streams {
        assert_eq!(
            restarted
                .loaded_stream_of_the_id(stream_id)
                .unwrap()
                .status(),
            RuntimeStatus::Paused,
            "pause is recorded, so a restart brings the project back paused"
        );
    }
}
