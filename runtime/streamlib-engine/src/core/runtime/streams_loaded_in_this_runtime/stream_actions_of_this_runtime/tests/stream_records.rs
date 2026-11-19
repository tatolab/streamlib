// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

#[test]
#[serial]
fn a_stopped_recorded_stream_is_held_stopped_and_a_fresh_runtime_holds_it_stopped_again() {
    let state_directory = tempfile::tempdir().unwrap();
    let project = ProjectWithGraphDocuments::holding(the_graph_of_a_stream_named("camera"));
    let camera = project.stream_id_of("camera");
    {
        let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
        let running = a_recorded_stream_loaded_without_its_start(
            &runner,
            state_directory.path(),
            &a_record_of(&project, "camera"),
        );

        let stopped = runner
            .stop_the_stream_finishing_its_pending_stop(&camera, None)
            .expect("the recorded stream stops");

        assert_eq!(
            stopped,
            StreamStopOutcome {
                stream_id: camera.clone(),
                stop_not_recorded_because: None,
            }
        );
        assert!(!running.has_ended());
        assert!(running.is_held_stopped());
        assert_eq!(
            runner.ids_of_the_loaded_streams(),
            std::slice::from_ref(&camera)
        );
    }

    let fresh_runner = a_runner_recording_its_streams_in(Some(state_directory.path()));

    let reloads = fresh_runner.reload_every_recorded_stream_not_closed_at_once();
    assert!(
        matches!(
            reloads.as_slice(),
            [StreamReloadAtTheStart::HeldStopped { stream_id }] if *stream_id == camera
        ),
        "{reloads:?}"
    );
    let held_again = fresh_runner.loaded_stream_of_the_id(&camera).unwrap();
    assert!(held_again.is_held_stopped());
    assert!(held_again.runtime_context.lock().is_none());
    let listings = fresh_runner.list_streams();
    assert_eq!(listings.len(), 1, "{listings:?}");
    assert_eq!(listings[0].state, StreamListingState::Stopped);
    assert_eq!(listings[0].node_count, Some(held_again.node_count()));
    assert!(the_record_of(state_directory.path(), &camera).stopped);
}

/// Overwrite the record of `stream_id` in `streams_directory` with a file
/// of each unreadable kind in turn — not JSON, then a record of a later
/// schema version — handing each one's bytes to `observe`.
fn for_each_unreadable_record_of(
    streams_directory: &Path,
    stream_id: &str,
    mut observe: impl FnMut(&Path, &[u8]),
) {
    let record_path = records_in(streams_directory)
        .record_path_of(stream_id)
        .unwrap();
    let mut later_schema =
        serde_json::to_value(the_record_of(streams_directory, stream_id)).unwrap();
    later_schema["schema_version"] = serde_json::json!(STREAM_RECORD_SCHEMA_VERSION + 1);
    for unreadable_bytes in [
        b"{ not json".to_vec(),
        serde_json::to_vec(&later_schema).unwrap(),
    ] {
        std::fs::write(&record_path, &unreadable_bytes).unwrap();
        observe(&record_path, &unreadable_bytes);
    }
}

#[test]
#[serial]
fn stopping_a_loaded_recorded_stream_whose_record_is_unreadable_unloads_it_naming_the_record() {
    let state_directory = tempfile::tempdir().unwrap();
    let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
    let project = ProjectWithGraphDocuments::with_no_venv();
    let camera = project.stream_id_of("camera");
    let record = a_record_of(&project, "camera");
    records_in(state_directory.path()).write(&record).unwrap();

    for_each_unreadable_record_of(
        state_directory.path(),
        &camera,
        |record_path, unreadable_bytes| {
            let running = a_stream_loaded_from_an_unwritten_record(&runner, &record);

            let stopped = runner
                .stop_the_stream_finishing_its_pending_stop(&camera, None)
                .expect("the stop succeeds, since the stream is unloaded");

            assert_eq!(stopped.stream_id, camera);
            let not_recorded = stopped
                .stop_not_recorded_because
                .expect("the outcome says the stop was not recorded");
            assert!(
                not_recorded.contains(&record_path.display().to_string()),
                "{not_recorded}"
            );
            assert!(running.has_ended());
            assert!(runner.ids_of_the_loaded_streams().is_empty());
            assert_eq!(
                std::fs::read(record_path).unwrap(),
                unreadable_bytes,
                "the unreadable record is left as it was"
            );

            let refusal =
                refusal_of(runner.stop_the_stream_finishing_its_pending_stop(&camera, None));
            assert!(
                refusal.contains(&record_path.display().to_string()),
                "with nothing loaded, the refusal names the record: {refusal}"
            );
        },
    );
}

#[test]
#[serial]
fn stopping_a_loaded_recorded_stream_whose_record_cannot_be_written_still_ends_its_playback() {
    let state_directory = tempfile::tempdir().unwrap();
    let streams_directory = state_directory.path().join("streams");
    let runner = a_runner_recording_its_streams_in(Some(&streams_directory));
    let project = ProjectWithGraphDocuments::with_no_venv();
    let camera = project.stream_id_of("camera");
    let running = a_recorded_stream_loaded_without_its_start(
        &runner,
        &streams_directory,
        &a_record_of(&project, "camera"),
    );

    let stopped = {
        let _refusing_writes = StreamsDirectoryRefusingWrites::from_now_on(&streams_directory);
        runner.stop_the_stream_finishing_its_pending_stop(&camera, None)
    }
    .expect("the stop succeeds, since the stream's playback ends");
    assert!(
        stopped.stop_not_recorded_because.is_some(),
        "the outcome says the stop was not recorded: {stopped:?}"
    );
    assert!(!running.has_ended());
    assert!(running.is_held_stopped());
    assert!(!the_record_of(&streams_directory, &camera).stopped);
}

#[test]
#[serial]
fn stopping_a_stream_its_host_loaded_unloads_it_and_records_nothing() {
    let state_directory = tempfile::tempdir().unwrap();
    let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
    let project = ProjectWithGraphDocuments::with_no_venv();
    a_stream_its_host_loaded(&runner, &project, "camera");

    let stopped = runner
        .stop_the_stream_finishing_its_pending_stop(&project.stream_id_of("camera"), None)
        .expect("the stream its host loaded stops");

    assert_eq!(stopped.stop_not_recorded_because, None);
    assert!(runner.ids_of_the_loaded_streams().is_empty());
    assert!(records_in(state_directory.path()).read_every().is_empty());
}

#[test]
#[serial]
fn an_action_naming_no_stream_the_runtime_holds_is_refused_naming_the_loaded_and_recorded_ones() {
    let state_directory = tempfile::tempdir().unwrap();
    let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
    let project = ProjectWithGraphDocuments::with_no_venv();
    a_stream_its_host_loaded(&runner, &project, "hosted-one");
    let mut stopped = a_record_of(&project, "recorded-one");
    stopped.stopped = true;
    records_in(state_directory.path()).write(&stopped).unwrap();
    let hosted_one = project.stream_id_of("hosted-one");
    let recorded_one = project.stream_id_of("recorded-one");

    let refusals = [
        refusal_of(runner.stop_the_stream_finishing_its_pending_stop("absent", None)),
        refusal_of(runner.remove_one_stream("absent")),
    ];

    for refusal in refusals {
        assert!(refusal.contains("`absent`"), "{refusal}");
        assert!(refusal.contains(&hosted_one), "{refusal}");
        assert!(
            refusal.contains(&format!("{recorded_one} (stopped)")),
            "{refusal}"
        );
    }
}

#[test]
#[serial]
fn a_stream_action_finds_a_stream_by_its_exact_id_alone() {
    let state_directory = tempfile::tempdir().unwrap();
    let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
    let project = ProjectWithGraphDocuments::with_no_venv();
    let hosted = a_stream_its_host_loaded(&runner, &project, "camera");
    let camera = project.stream_id_of("camera");
    let mut stopped = a_record_of(&project, "microphone");
    stopped.stopped = true;
    records_in(state_directory.path()).write(&stopped).unwrap();
    let microphone = project.stream_id_of("microphone");

    for spelled_otherwise in [
        camera.to_uppercase(),
        format!(" {camera}"),
        "camera".to_string(),
    ] {
        let refusal =
            refusal_of(runner.stop_the_stream_finishing_its_pending_stop(&spelled_otherwise, None));

        assert!(refusal.contains("no stream with the id"), "{refusal}");
    }
    assert!(!hosted.has_ended());
    assert_eq!(runner.ids_of_the_loaded_streams(), [camera]);
    assert_eq!(
        the_current_record_in(state_directory.path(), &microphone),
        Some(stopped)
    );
}

#[test]
#[serial]
fn removing_a_recorded_stream_unloads_it_and_forgets_its_record() {
    let state_directory = tempfile::tempdir().unwrap();
    let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
    let project = ProjectWithGraphDocuments::with_no_venv();
    let camera = project.stream_id_of("camera");
    let microphone = project.stream_id_of("microphone");
    let running = a_recorded_stream_loaded_without_its_start(
        &runner,
        state_directory.path(),
        &a_record_of(&project, "camera"),
    );
    let mut stopped = a_record_of(&project, "microphone");
    stopped.stopped = true;
    records_in(state_directory.path()).write(&stopped).unwrap();

    assert_eq!(
        runner.remove_one_stream(&camera).unwrap(),
        StreamRemoveOutcome {
            stream_id: camera.clone(),
            unloaded: true,
            forgotten: true,
        }
    );
    assert_eq!(
        runner.remove_one_stream(&microphone).unwrap(),
        StreamRemoveOutcome {
            stream_id: microphone.clone(),
            unloaded: false,
            forgotten: true,
        }
    );

    assert!(running.has_ended());
    assert!(runner.list_streams().is_empty());
    assert!(records_in(state_directory.path()).read_every().is_empty());
}

#[test]
#[serial]
fn a_display_name_its_graph_document_writes_shows_a_stream_loaded_or_not_and_after_a_restart() {
    let state_directory = tempfile::tempdir().unwrap();
    let project = ProjectWithGraphDocuments::with_no_venv();
    for (stream_name, display_name) in [
        ("camera", "Hello camera"),
        ("microphone", "  Desk mic ✨  "),
    ] {
        let mut graph = the_graph_of_a_stream_named(stream_name);
        graph["display_name"] = serde_json::json!(display_name);
        project.write_the_graph_document(graph);
    }
    let camera = project.stream_id_of("camera");
    let microphone = project.stream_id_of("microphone");
    let mut expected = vec![
        (camera.clone(), "Hello camera".to_string()),
        (microphone, "  Desk mic ✨  ".to_string()),
    ];
    expected.sort_by(|first, second| first.0.cmp(&second.0));
    let shown_as = |runner: &Runner| -> Vec<(String, String)> {
        runner
            .list_streams()
            .into_iter()
            .map(|listing| (listing.stream_id, listing.display_name))
            .collect()
    };
    {
        let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
        let running = a_recorded_stream_loaded_without_its_start(
            &runner,
            state_directory.path(),
            &a_record_of(&project, "camera"),
        );
        let mut stopped = a_record_of(&project, "microphone");
        stopped.stopped = true;
        records_in(state_directory.path()).write(&stopped).unwrap();

        assert_eq!(running.display_name(), "Hello camera");
        assert_eq!(shown_as(&runner), expected);
    }

    let fresh_runner = a_runner_recording_its_streams_in(Some(state_directory.path()));

    assert_eq!(shown_as(&fresh_runner), expected);
}

#[test]
#[serial]
fn a_loaded_stream_is_shown_under_the_name_its_stream_asset_is_given_and_keeps_its_id() {
    let state_directory = tempfile::tempdir().unwrap();
    let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
    let project = ProjectWithGraphDocuments::with_no_venv();
    project.write_the_graph_document(the_graph_of_a_stream_named("camera"));
    let camera = project.stream_id_of("camera");
    let running = a_recorded_stream_loaded_without_its_start(
        &runner,
        state_directory.path(),
        &a_record_of(&project, "camera"),
    );

    let shown = runner.show_the_stream_of_a_stream_asset_under(
        project.project,
        project.stream_asset_of("camera"),
        "Hello camera",
    );

    assert_eq!(shown, Some(camera.clone()));
    assert_eq!(running.display_name(), "Hello camera");
    assert_eq!(
        running.stream_id(),
        camera,
        "a new name never changes the id"
    );
    assert_eq!(
        runner.show_the_stream_of_a_stream_asset_under(
            project.project,
            project.stream_asset_of("microphone"),
            "Desk mic",
        ),
        None,
        "a stream asset no loaded stream runs has no stream to show"
    );
}

#[test]
#[serial]
fn the_listing_names_each_stream_by_id_display_name_and_location_and_one_not_loaded_counts_no_node()
{
    let state_directory = tempfile::tempdir().unwrap();
    let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
    let project = ProjectWithGraphDocuments::with_no_venv();
    for recorded in ["b-recorded", "c-stopped", "d-not-re-loaded"] {
        project.write_the_graph_document(the_graph_of_a_stream_named(recorded));
    }
    a_stream_its_host_loaded(&runner, &project, "a-hosted");
    a_recorded_stream_loaded_without_its_start(
        &runner,
        state_directory.path(),
        &a_record_of(&project, "b-recorded"),
    );
    let mut stopped = a_record_of(&project, "c-stopped");
    stopped.stopped = true;
    records_in(state_directory.path()).write(&stopped).unwrap();
    records_in(state_directory.path())
        .write(&a_record_of(&project, "d-not-re-loaded"))
        .unwrap();

    let listing_of = |function_name: &str, state, node_count, condition| StreamListing {
        stream_id: project.stream_id_of(function_name),
        display_name: function_name.to_string(),
        stream_location: Some(project.location_of(function_name)),
        state,
        node_count,
        failed_because: None,
        condition,
        capture_devices_whose_access_was_never_asked_for: Vec::new(),
        stream_action_under_way: None,
    };
    let mut expected = [
        StreamListing {
            display_name: project.stream_id_of("a-hosted"),
            ..listing_of(
                "a-hosted",
                StreamListingState::Running,
                Some(1),
                a_healthy_condition(
                    StreamLoadOnTheWire::Loaded,
                    StreamPlaybackOnTheWire::Playing,
                ),
            )
        },
        listing_of(
            "b-recorded",
            StreamListingState::Running,
            Some(1),
            a_healthy_condition(
                StreamLoadOnTheWire::Loaded,
                StreamPlaybackOnTheWire::Playing,
            ),
        ),
        listing_of(
            "c-stopped",
            StreamListingState::Stopped,
            None,
            a_healthy_condition(
                StreamLoadOnTheWire::NotLoaded,
                StreamPlaybackOnTheWire::Stopped,
            ),
        ),
        listing_of(
            "d-not-re-loaded",
            StreamListingState::Running,
            None,
            a_healthy_condition(
                StreamLoadOnTheWire::NotLoaded,
                StreamPlaybackOnTheWire::Playing,
            ),
        ),
    ];
    expected.sort_by(|first, second| first.stream_id.cmp(&second.stream_id));
    assert_eq!(runner.list_streams(), expected);
}

#[test]
#[serial]
fn a_record_an_earlier_runtime_wrote_is_listed_failed_never_loaded_and_removed_by_its_file_name() {
    let state_directory = tempfile::tempdir().unwrap();
    let earlier_record_path = state_directory.path().join("main.json");
    let earlier_record_bytes = serde_json::to_vec(&serde_json::json!({
        "schema_version": 2,
        "stream_name": "main",
        "project_directory": "/home/someone/hello",
        "interpreter": "/home/someone/hello/.venv/bin/python",
        "stream_function": null,
        "graph": {"stream": "main", "nodes": [], "links": []},
        "stopped": false,
        "failed_because": null,
        "runtime_crashes_in_a_row_implicating_it": 0,
        "exposure_rulings": [],
    }))
    .unwrap();
    std::fs::write(&earlier_record_path, &earlier_record_bytes).unwrap();
    let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));

    runner.count_the_previous_runtime_runs_end_against_the_recorded_streams(&a_crash_pinned_on(&[
        "main",
    ]));
    let reloads = runner.reload_every_recorded_stream_not_closed_at_once();
    let listing = runner.list_streams();

    assert_eq!(
        std::fs::read(&earlier_record_path).unwrap(),
        earlier_record_bytes,
        "neither the crash count nor the re-load rewrites it"
    );
    let [
        StreamReloadAtTheStart::SkippedAsFailed {
            stream_id,
            failed_because,
        },
    ] = reloads.as_slice()
    else {
        panic!("the record is skipped as failed: {reloads:?}");
    };
    assert_eq!(stream_id, "main");
    assert!(
        failed_because.contains("an earlier runtime recorded it"),
        "{failed_because}"
    );
    let [listed] = listing.as_slice() else {
        panic!("the one record is listed: {listing:?}");
    };
    assert_eq!(listed.stream_id, "main");
    assert_eq!(listed.display_name, "main");
    assert_eq!(listed.stream_location, None);
    assert_eq!(listed.state, StreamListingState::Failed);
    assert_eq!(listed.node_count, None);
    let listed_failed_because = listed.failed_because.as_deref().unwrap_or_default();
    assert!(
        listed_failed_because.contains("an earlier runtime recorded it")
            && listed_failed_because.contains("/home/someone/hello"),
        "{listed_failed_because}"
    );
    let refusal = refusal_of(runner.stop_the_stream_finishing_its_pending_stop("main", None));
    assert!(
        refusal.contains("an earlier runtime recorded it"),
        "{refusal}"
    );
    assert!(runner.ids_of_the_loaded_streams().is_empty());

    assert_eq!(
        runner.remove_one_stream("main").unwrap(),
        StreamRemoveOutcome {
            stream_id: "main".to_string(),
            unloaded: false,
            forgotten: true,
        }
    );
    assert!(!earlier_record_path.exists());
    assert!(runner.list_streams().is_empty());
}

#[test]
#[serial]
fn a_run_leaves_a_stream_its_host_loaded_and_forgets_a_record_whose_document_is_gone() {
    let state_directory = tempfile::tempdir().unwrap();
    let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
    let project = ProjectWithGraphDocuments::holding(the_graph_of_a_stream_named("studio"));
    project.write_an_unloadable_graph_document("studio");
    let hosted = a_stream_its_host_loaded(&runner, &project, "camera");
    let record = a_record_of(&project, "recorded-camera");
    let recorded =
        a_recorded_stream_loaded_without_its_start(&runner, state_directory.path(), &record);

    let run = runner
        .load_the_project_off_its_queue(project.run_request())
        .unwrap();

    let (stream, _failed_because) = the_one_failed_stream_of(&run);
    assert_eq!(stream.stream_id, project.stream_id_of("studio"));
    assert!(!hosted.has_ended());
    assert!(recorded.has_ended());
    assert_eq!(
        runner.ids_of_the_loaded_streams(),
        [project.stream_id_of("camera")]
    );
    assert_eq!(
        the_current_record_in(state_directory.path(), &record.stream_id),
        None
    );
}

// The tests below start streams, which needs the engine's GPU context.

#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
#[serial]
fn two_projects_of_one_stream_name_run_side_by_side_under_two_ids_and_neither_is_refused() {
    let state_directory = tempfile::tempdir().unwrap();
    let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
    let main_worktree = ProjectWithGraphDocuments::holding(the_graph_of_a_stream_named("main"));
    let second_worktree = ProjectWithGraphDocuments::holding(the_graph_of_a_stream_named("main"));

    let first_run = runner
        .load_the_project_off_its_queue(main_worktree.run_request())
        .expect("the first worktree's stream runs");
    let second_run = runner
        .load_the_project_off_its_queue(second_worktree.run_request())
        .expect("the second worktree's stream runs beside it");
    let (first, _, _) = the_one_running_stream_of(&first_run);
    let (second, _, _) = the_one_running_stream_of(&second_run);

    assert_eq!(first.stream_id, main_worktree.stream_id_of("main"));
    assert_eq!(second.stream_id, second_worktree.stream_id_of("main"));
    assert_ne!(first.stream_id, second.stream_id);
    assert_eq!(
        (first.display_name.as_str(), second.display_name.as_str()),
        ("main", "main")
    );
    let mut loaded_ids = runner.ids_of_the_loaded_streams();
    loaded_ids.sort();
    let mut expected_ids = vec![first.stream_id.clone(), second.stream_id.clone()];
    expected_ids.sort();
    assert_eq!(loaded_ids, expected_ids);
    assert!(the_current_record_in(state_directory.path(), &first.stream_id).is_some());
    assert!(the_current_record_in(state_directory.path(), &second.stream_id).is_some());

    let rerun = runner
        .load_the_project_off_its_queue(main_worktree.run_request())
        .expect("the first worktree runs again");
    let (rerun, _, _) = the_one_running_stream_of(&rerun);
    assert_eq!(rerun.stream_id, first.stream_id, "a re-run keeps its id");
    assert!(rerun.replaced_a_recorded_stream);
    assert_eq!(records_in(state_directory.path()).read_every().len(), 2);
}

#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
#[serial]
fn a_run_starts_the_stream_under_its_locations_id_and_records_it() {
    let state_directory = tempfile::tempdir().unwrap();
    let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
    let camera_project = ProjectWithGraphDocuments::holding(the_graph_of_a_stream_named("camera"));
    let preview_project =
        ProjectWithGraphDocuments::holding(the_graph_of_a_stream_named("preview"));
    let camera = camera_project.stream_id_of("camera");
    let preview = preview_project.stream_id_of("preview");

    let camera_outcome = runner
        .load_the_project_off_its_queue(camera_project.run_request())
        .expect("the camera stream runs");
    let preview_outcome = runner
        .load_the_project_off_its_queue(preview_project.run_request())
        .expect("the preview stream runs");
    let (camera_run, _, camera_node_count) = the_one_running_stream_of(&camera_outcome);
    let (preview_run, _, _) = the_one_running_stream_of(&preview_outcome);

    assert_eq!(camera_run.stream_id, camera);
    assert_eq!(
        camera_run.display_name, "camera",
        "its graph document's name by default"
    );
    assert_eq!(
        camera_run.stream_location,
        camera_project.location_of("camera")
    );
    assert_eq!(camera_node_count, 1);
    assert!(!camera_run.replaced_a_recorded_stream);
    assert_eq!(camera_outcome.project_directory, camera_project.path());
    assert_eq!(preview_run.stream_id, preview);
    assert_eq!(preview_run.display_name, "preview");
    assert_eq!(
        the_current_record_in(state_directory.path(), &camera),
        Some(a_record_of(&camera_project, "camera"))
    );
    assert_eq!(
        the_current_record_in(state_directory.path(), &preview),
        Some(a_record_of(&preview_project, "preview"))
    );
    let mut expected_listing = [
        StreamListing {
            stream_id: camera.clone(),
            display_name: "camera".to_string(),
            stream_location: Some(camera_project.location_of("camera")),
            state: StreamListingState::Running,
            node_count: Some(1),
            failed_because: None,
            condition: a_healthy_condition(
                StreamLoadOnTheWire::Loaded,
                StreamPlaybackOnTheWire::Playing,
            ),
            capture_devices_whose_access_was_never_asked_for: Vec::new(),
            stream_action_under_way: None,
        },
        StreamListing {
            stream_id: preview.clone(),
            display_name: "preview".to_string(),
            stream_location: Some(preview_project.location_of("preview")),
            state: StreamListingState::Running,
            node_count: Some(1),
            failed_because: None,
            condition: a_healthy_condition(
                StreamLoadOnTheWire::Loaded,
                StreamPlaybackOnTheWire::Playing,
            ),
            capture_devices_whose_access_was_never_asked_for: Vec::new(),
            stream_action_under_way: None,
        },
    ];
    expected_listing.sort_by(|first, second| first.stream_id.cmp(&second.stream_id));
    assert_eq!(runner.list_streams(), expected_listing);
    let running = runner.loaded_stream_of_the_id(&camera).unwrap();
    assert_eq!(
        running.status(),
        crate::core::runtime::RuntimeStatus::Started
    );
    assert_eq!(running.holding(), LoadedStreamHolding::RecordedByTheRuntime);
    assert_eq!(
        running.to_json().unwrap()["stream"],
        serde_json::json!(camera),
        "the loaded graph's stream is the id"
    );
}

#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
#[serial]
fn a_run_of_the_records_location_replaces_it_under_its_id_shown_as_its_document_says_now() {
    let state_directory = tempfile::tempdir().unwrap();
    let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
    let project = ProjectWithGraphDocuments::holding(the_graph_of_a_stream_named("camera"));
    let camera = project.stream_id_of("camera");
    let first_run = runner
        .load_the_project_off_its_queue(project.run_request())
        .unwrap();
    let (first, first_stream_tag, _) = the_one_running_stream_of(&first_run);
    let mut changed_source = the_graph_of_a_stream_named("camera");
    changed_source["display_name"] = serde_json::json!("Front camera");
    project.write_the_graph_document(changed_source);

    let replaced_run = runner
        .load_the_project_off_its_queue(project.run_request())
        .expect("the recorded stream is replaced");
    let (replaced, replaced_stream_tag, _) = the_one_running_stream_of(&replaced_run);

    assert_eq!(
        first.display_name, "camera",
        "a new stream is shown under its graph document's name"
    );
    assert!(replaced.replaced_a_recorded_stream);
    assert_eq!(replaced.stream_id, camera);
    assert_eq!(
        replaced.display_name, "Front camera",
        "a load shows the display name its document writes"
    );
    assert_ne!(replaced_stream_tag, first_stream_tag);
    let running = runner.loaded_stream_of_the_id(&camera).unwrap();
    assert_eq!(running.stream_tag(), replaced_stream_tag);
    assert_eq!(running.display_name(), "Front camera");

    assert_eq!(records_in(state_directory.path()).read_every().len(), 1);
}

#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
#[serial]
fn a_stopped_project_runs_again_and_re_loads_at_a_restart() {
    let state_directory = tempfile::tempdir().unwrap();
    let project = ProjectWithGraphDocuments::holding(the_graph_of_a_stream_named("camera"));
    let camera = project.stream_id_of("camera");
    {
        let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
        runner
            .load_the_project_off_its_queue(project.run_request())
            .unwrap();
        runner
            .stop_the_stream_finishing_its_pending_stop(&camera, None)
            .unwrap();

        let run = runner
            .load_the_project_off_its_queue(project.run_request())
            .expect("the stopped project runs again");

        let (stream, _, node_count) = the_one_running_stream_of(&run);
        assert_eq!(stream.stream_id, camera);
        assert_eq!(node_count, 1);
        assert!(runner.loaded_stream_of_the_id(&camera).is_ok());
        assert!(!the_record_of(state_directory.path(), &camera).stopped);
    }

    let restarted = a_runner_recording_its_streams_in(Some(state_directory.path()));
    let reloads = restarted.reload_every_recorded_stream_not_closed_at_once();

    assert!(
        matches!(
            reloads.as_slice(),
            [StreamReloadAtTheStart::Reloaded { stream_id }] if *stream_id == camera
        ),
        "{reloads:?}"
    );
    assert!(restarted.loaded_stream_of_the_id(&camera).is_ok());
}

#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
#[serial]
fn a_run_records_its_stream_before_the_start_and_a_refused_start_holds_it_stopped_recorded_failed()
{
    let state_directory = tempfile::tempdir().unwrap();
    let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
    let records_seen_by_the_start = Arc::new(Mutex::new(None));
    let records_the_start_saw = Arc::clone(&records_seen_by_the_start);
    let streams_directory = state_directory.path().to_path_buf();
    runner
        .install_setup_hook(move |_| {
            *records_the_start_saw.lock() = Some(records_in(&streams_directory).read_every().len());
            Err(Error::Configuration("the setup hook refuses".into()))
        })
        .unwrap();
    let project = ProjectWithGraphDocuments::holding(the_graph_of_a_stream_named("camera"));

    let run = runner
        .load_the_project_off_its_queue(project.run_request())
        .expect("a refused start is a failed stream, not a refused run");

    let (stream, failed_because) = the_one_failed_stream_of(&run);
    assert!(
        failed_because.contains("the setup hook refuses"),
        "{failed_because}"
    );
    assert_eq!(
        *records_seen_by_the_start.lock(),
        Some(1),
        "the stream is recorded before its start, so a crash in the start is counted"
    );
    assert!(
        runner
            .loaded_stream_of_the_id(&stream.stream_id)
            .unwrap()
            .is_held_stopped(),
        "the stream whose start was refused stays held"
    );
    let recorded = the_record_of(state_directory.path(), &stream.stream_id);
    assert_eq!(recorded.failed_because.as_deref(), Some(failed_because));
}
