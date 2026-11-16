// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

#[test]
#[serial]
fn the_reload_at_the_start_reads_each_records_graph_document_and_fails_one_that_does_not_load() {
    let state_directory = tempfile::tempdir().unwrap();
    let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
    let project = ProjectWithGraphDocuments::with_no_venv().given_a_venv();
    project.hold_the_streams(&["camera"], &["broken"]);
    for stream_name in ["camera", "broken"] {
        records_in(state_directory.path())
            .write(&a_record_of(&project, stream_name))
            .unwrap();
    }
    let broken = project.stream_id_of("broken");

    let reloads = runner.reload_every_recorded_stream_not_closed_at_once();

    assert!(
        reloads.iter().any(|reload| matches!(
            reload,
            StreamReloadAtTheStart::Reloaded { stream_id }
                if *stream_id == project.stream_id_of("camera")
        )),
        "{reloads:?}"
    );
    assert!(
        reloads.iter().any(|reload| matches!(
            reload,
            StreamReloadAtTheStart::NotReloaded { stream_id, refusal }
                if *stream_id == broken && refusal.to_string().contains("does not load")
        )),
        "{reloads:?}"
    );
    assert!(
        the_record_of(state_directory.path(), &broken)
            .failed_because
            .is_some_and(|failed_because| failed_because.contains("does not load"))
    );
}

#[test]
#[serial]
fn reloading_holds_each_recorded_stream_whose_venv_cannot_be_made_failed_and_keeps_going() {
    let state_directory = tempfile::tempdir().unwrap();
    let project = ProjectWithGraphDocuments::with_no_venv();
    let records = records_in(state_directory.path());
    for function_name in ["a-camera", "c-microphone"] {
        project.write_the_graph_document(the_graph_of_a_stream_named(function_name));
        records
            .write(&a_record_of(&project, function_name))
            .unwrap();
    }
    let malformed = state_directory.path().join("b-malformed.json");
    std::fs::write(&malformed, b"{").unwrap();
    let mut closed = a_record_of(&project, "d-closed");
    closed.mark_closed();
    records.write(&closed).unwrap();
    let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));

    let reloads = runner.reload_every_recorded_stream_not_closed_at_once();

    assert_eq!(reloads.len(), 3, "{reloads:?}");
    let mut ids_not_reloaded = Vec::new();
    let mut unreadable_paths = Vec::new();
    for reload in &reloads {
        match reload {
            StreamReloadAtTheStart::HeldStopped { stream_id } => {
                let failed_because = records
                    .read(stream_id)
                    .ok()
                    .flatten()
                    .and_then(|read| match read {
                        StreamRecordAsRead::Current(record) => record.failed_because,
                        StreamRecordAsRead::RecordedByAnEarlierRuntime(_) => None,
                    })
                    .expect("the stream is recorded failed");
                assert!(
                    failed_because.contains(&project.interpreter().display().to_string()),
                    "{failed_because}"
                );
                assert!(failed_because.contains("carries no uv"), "{failed_because}");
                ids_not_reloaded.push(stream_id.clone());
            }
            StreamReloadAtTheStart::RecordUnreadable(unreadable) => {
                unreadable_paths.push(unreadable.path.clone());
            }
            other => {
                panic!("only the two recorded streams and the malformed record: {other:?}")
            }
        }
    }
    ids_not_reloaded.sort();
    let mut expected_ids = vec![
        project.stream_id_of("a-camera"),
        project.stream_id_of("c-microphone"),
    ];
    expected_ids.sort();
    assert_eq!(ids_not_reloaded, expected_ids);
    assert_eq!(unreadable_paths, [malformed]);
    let mut loaded = runner.ids_of_the_loaded_streams();
    loaded.sort();
    assert_eq!(loaded, expected_ids, "each is held to read and edit");
    assert_eq!(records.read_every().len(), 4, "every record stays");
}

#[test]
#[serial]
fn a_recorded_stream_closed_or_removed_between_the_listing_and_its_reload_stays_unloaded() {
    let state_directory = tempfile::tempdir().unwrap();
    let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
    let project = ProjectWithGraphDocuments::holding(the_graph_of_a_stream_named("camera"));
    let records = records_in(state_directory.path());
    for function_name in ["a-closed", "b-removed"] {
        records
            .write(&a_record_of(&project, function_name))
            .unwrap();
    }
    let listed = records.read_every();
    let mut closed_since = the_record_of(state_directory.path(), &project.stream_id_of("a-closed"));
    closed_since.mark_closed();
    records.write(&closed_since).unwrap();
    assert!(records.remove(&project.stream_id_of("b-removed")).unwrap());

    let reloads = runner.reload_each_recorded_stream_listed(
        &records,
        listed,
        &is_the_machines_shutdown_requested,
    );

    assert!(reloads.is_empty(), "neither is reported: {reloads:?}");
    assert!(runner.ids_of_the_loaded_streams().is_empty());
    assert_eq!(
        records.read_every(),
        vec![Ok(StreamRecordAsRead::Current(closed_since))]
    );
}

#[test]
#[serial]
fn a_record_unreadable_by_its_reload_is_reported_unreadable_by_its_path() {
    let state_directory = tempfile::tempdir().unwrap();
    let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
    let project = ProjectWithGraphDocuments::with_no_venv();
    let records = records_in(state_directory.path());
    records.write(&a_record_of(&project, "camera")).unwrap();
    let listed = records.read_every();
    let record_path_malformed_since_the_listing = records
        .record_path_of(&project.stream_id_of("camera"))
        .unwrap();
    std::fs::write(&record_path_malformed_since_the_listing, b"{").unwrap();

    let reloads = runner.reload_each_recorded_stream_listed(
        &records,
        listed,
        &is_the_machines_shutdown_requested,
    );

    let [StreamReloadAtTheStart::RecordUnreadable(unreadable)] = reloads.as_slice() else {
        panic!("the record is reported unreadable: {reloads:?}");
    };
    assert_eq!(unreadable.path, record_path_malformed_since_the_listing);
    assert!(runner.ids_of_the_loaded_streams().is_empty());
    assert!(
        record_path_malformed_since_the_listing.is_file(),
        "the record is left in place"
    );
}

#[test]
#[serial]
fn a_recorded_stream_loaded_between_the_listing_and_its_reload_is_left_as_it_is() {
    let state_directory = tempfile::tempdir().unwrap();
    let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
    let project = ProjectWithGraphDocuments::with_no_venv();
    let record = a_record_of(&project, "camera");
    records_in(state_directory.path()).write(&record).unwrap();
    let listed = records_in(state_directory.path()).read_every();
    let loaded_since =
        a_recorded_stream_loaded_without_its_start(&runner, state_directory.path(), &record);

    let reloads = runner.reload_each_recorded_stream_listed(
        &records_in(state_directory.path()),
        listed,
        &is_the_machines_shutdown_requested,
    );

    assert!(reloads.is_empty(), "nothing is reported: {reloads:?}");
    assert_eq!(
        runner
            .loaded_stream_of_the_id(&record.stream_id)
            .unwrap()
            .stream_tag(),
        loaded_since.stream_tag()
    );
    assert!(!loaded_since.has_ended());
}

#[test]
#[serial]
fn a_recorded_stream_implicated_in_two_crashes_in_a_row_is_failed_naming_the_last_cause() {
    let state_directory = tempfile::tempdir().unwrap();
    let project = ProjectWithGraphDocuments::with_no_venv().given_a_venv();
    for function_name in ["crasher", "steady"] {
        records_in(state_directory.path())
            .write(&a_record_of(&project, function_name))
            .unwrap();
    }
    let crasher = project.stream_id_of("crasher");
    let steady = project.stream_id_of("steady");
    let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));

    let failed_by_the_first = runner
        .count_the_previous_runtime_runs_end_against_the_recorded_streams(&a_crash_pinned_on(&[
            &crasher,
        ]));

    assert!(failed_by_the_first.is_empty(), "{failed_by_the_first:?}");
    let crasher_record = the_record_of(state_directory.path(), &crasher);
    assert_eq!(crasher_record.runtime_crashes_in_a_row_implicating_it, 1);
    assert!(!crasher_record.is_failed());

    let failed_by_the_second = runner
        .count_the_previous_runtime_runs_end_against_the_recorded_streams(&a_crash_pinned_on(&[
            &crasher,
        ]));

    assert_eq!(failed_by_the_second, std::slice::from_ref(&crasher));
    let crasher_record = the_record_of(state_directory.path(), &crasher);
    assert_eq!(crasher_record.runtime_crashes_in_a_row_implicating_it, 2);
    let failed_because = crasher_record
        .failed_because
        .expect("the crasher is failed");
    assert!(
        failed_because.contains("last 2 crashes in a row"),
        "{failed_because}"
    );
    assert!(failed_because.contains("SIGSEGV"), "{failed_because}");
    let steady_record = the_record_of(state_directory.path(), &steady);
    assert_eq!(steady_record.runtime_crashes_in_a_row_implicating_it, 0);
    assert!(!steady_record.is_failed());
    assert_eq!(
        runner
            .list_streams()
            .into_iter()
            .map(|listing| (
                listing.stream_id,
                listing.state,
                listing.failed_because.is_some()
            ))
            .collect::<Vec<_>>(),
        [
            (crasher, StreamListingState::Failed, true),
            (steady, StreamListingState::Running, false),
        ]
    );
}

#[test]
#[serial]
fn a_clean_end_or_a_crash_pinned_elsewhere_between_two_crashes_resets_the_count() {
    let state_directory = tempfile::tempdir().unwrap();
    let project = ProjectWithGraphDocuments::with_no_venv().given_a_venv();
    records_in(state_directory.path())
        .write(&a_record_of(&project, "crasher"))
        .unwrap();
    let crasher = project.stream_id_of("crasher");
    let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
    let a_crash_pinned_on_no_stream =
        HowThePreviousRuntimeRunEnded::Crashed(CrashOfThePreviousRuntimeRun {
            causes_by_implicated_stream: BTreeMap::new(),
            causes_on_threads_no_stream_owns: vec!["SIGSEGV".to_string()],
        });

    for previous_run_between_the_crashes in [
        HowThePreviousRuntimeRunEnded::Cleanly,
        a_crash_pinned_on(&["another-stream"]),
        a_crash_pinned_on_no_stream,
    ] {
        runner.count_the_previous_runtime_runs_end_against_the_recorded_streams(
            &a_crash_pinned_on(&[&crasher]),
        );
        runner.count_the_previous_runtime_runs_end_against_the_recorded_streams(
            &previous_run_between_the_crashes,
        );
        assert_eq!(
            the_record_of(state_directory.path(), &crasher).runtime_crashes_in_a_row_implicating_it,
            0,
            "{previous_run_between_the_crashes:?} did not reset the count"
        );
        let failed = runner.count_the_previous_runtime_runs_end_against_the_recorded_streams(
            &a_crash_pinned_on(&[&crasher]),
        );

        assert!(
            failed.is_empty(),
            "{previous_run_between_the_crashes:?}: {failed:?}"
        );
        assert!(!the_record_of(state_directory.path(), &crasher).is_failed());
        runner.count_the_previous_runtime_runs_end_against_the_recorded_streams(
            &HowThePreviousRuntimeRunEnded::Cleanly,
        );
    }
}

#[test]
#[serial]
fn a_stream_failed_for_crashing_the_runtime_is_held_at_the_start_reading_none_of_its_node_code() {
    let state_directory = tempfile::tempdir().unwrap();
    let project = ProjectWithGraphDocuments::with_no_venv().given_a_venv();
    project.write_the_graph_document(the_graph_of_a_stream_naming_a_type_not_described("crasher"));
    let crasher = project.stream_id_of("crasher");
    let mut failed = a_record_of(&project, "crasher");
    failed.failed_because = Some("it crashed the runtime".to_string());
    failed.runtime_crashes_in_a_row_implicating_it = RUNTIME_CRASHES_IN_A_ROW_THAT_FAIL_A_STREAM;
    records_in(state_directory.path()).write(&failed).unwrap();
    let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));

    runner.count_the_previous_runtime_runs_end_against_the_recorded_streams(
        &HowThePreviousRuntimeRunEnded::Cleanly,
    );
    let reloads = runner.reload_every_recorded_stream_not_closed_at_once();

    assert_eq!(the_record_of(state_directory.path(), &crasher), failed);
    assert!(
        matches!(
            reloads.as_slice(),
            [StreamReloadAtTheStart::HeldStopped { stream_id }] if *stream_id == crasher
        ),
        "{reloads:?}"
    );
    let held = runner.loaded_stream_of_the_id(&crasher).unwrap();
    assert!(held.is_held_stopped() && held.runtime_context.lock().is_none());
    assert_eq!(held.node_count(), 2);
    let source_type = MockOutputOnlyNode::Node::node_class_import_path();
    assert!(
        held.node_types_this_stream_resolves()
            .placeholder_of_the_node_type_not_described(&source_type)
            .is_some(),
        "a native node is held as a placeholder too, its config never checked"
    );
    assert!(
        held.why_its_graph_as_held_keeps_this_stream_from_playing()
            .is_some_and(|why| why.contains("none of its node code read")),
    );
    let listed = runner
        .list_streams()
        .into_iter()
        .find(|listed| listed.stream_id == crasher)
        .unwrap();
    assert_eq!(listed.state, StreamListingState::Failed);
    assert_eq!(
        listed.failed_because.as_deref(),
        Some(
            "it is held as its document is written, none of its node code read: it crashed the runtime"
        )
    );
}

#[test]
#[serial]
fn a_stream_that_crashed_the_runtime_even_held_from_its_document_is_not_loaded_at_the_start() {
    let state_directory = tempfile::tempdir().unwrap();
    let project = ProjectWithGraphDocuments::with_no_venv().given_a_venv();
    project.write_the_graph_document(the_graph_of_a_stream_named("crasher"));
    let crasher = project.stream_id_of("crasher");
    let mut failed = a_record_of(&project, "crasher");
    failed.failed_because = Some("it crashed the runtime".to_string());
    failed.runtime_crashes_in_a_row_implicating_it = RUNTIME_CRASHES_IN_A_ROW_THAT_FAIL_A_STREAM;
    records_in(state_directory.path()).write(&failed).unwrap();
    let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));

    runner.count_the_previous_runtime_runs_end_against_the_recorded_streams(
        &HowThePreviousRuntimeRunEnded::Crashed(CrashOfThePreviousRuntimeRun {
            causes_by_implicated_stream: [(crasher.clone(), "SIGSEGV".to_string())].into(),
            ..CrashOfThePreviousRuntimeRun::default()
        }),
    );
    runner.count_the_previous_runtime_runs_end_against_the_recorded_streams(
        &HowThePreviousRuntimeRunEnded::Cleanly,
    );
    let reloads = runner.reload_every_recorded_stream_not_closed_at_once();

    let recorded = the_record_of(state_directory.path(), &crasher);
    assert!(
        recorded.crashed_the_runtime_even_held_from_its_document(),
        "{recorded:?}"
    );
    assert!(
        matches!(
            reloads.as_slice(),
            [StreamReloadAtTheStart::SkippedAsFailed { stream_id, .. }] if *stream_id == crasher
        ),
        "{reloads:?}"
    );
    assert!(
        runner
            .loaded_stream_of_the_id_when_loaded(&crasher)
            .is_none()
    );
}

#[test]
#[serial]
fn a_stream_failed_for_a_reason_other_than_crashes_is_held_stopped_at_the_start() {
    let state_directory = tempfile::tempdir().unwrap();
    let project = ProjectWithGraphDocuments::with_no_venv().given_a_venv();
    project.write_the_graph_document(the_graph_of_a_stream_named("refused"));
    let refused = project.stream_id_of("refused");
    let mut failed = a_record_of(&project, "refused");
    failed.failed_because = Some("its start was refused".to_string());
    records_in(state_directory.path()).write(&failed).unwrap();
    let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));

    runner.count_the_previous_runtime_runs_end_against_the_recorded_streams(
        &HowThePreviousRuntimeRunEnded::Cleanly,
    );
    let reloads = runner.reload_every_recorded_stream_not_closed_at_once();

    assert_eq!(the_record_of(state_directory.path(), &refused), failed);
    assert!(
        matches!(
            reloads.as_slice(),
            [StreamReloadAtTheStart::HeldStopped { stream_id }] if *stream_id == refused
        ),
        "{reloads:?}"
    );
    let held = runner.loaded_stream_of_the_id(&refused).unwrap();
    assert!(held.is_held_stopped() && held.runtime_context.lock().is_none());
    let listed = runner
        .list_streams()
        .into_iter()
        .find(|listed| listed.stream_id == refused)
        .unwrap();
    assert_eq!(listed.state, StreamListingState::Failed);
    assert_eq!(
        listed.failed_because.as_deref(),
        Some("its start was refused")
    );
}

#[test]
#[serial]
fn a_stream_failed_for_another_reason_that_keeps_crashing_the_runtime_is_failed_for_its_crashes() {
    let state_directory = tempfile::tempdir().unwrap();
    let project = ProjectWithGraphDocuments::with_no_venv().given_a_venv();
    let refused = project.stream_id_of("refused");
    let mut failed = a_record_of(&project, "refused");
    failed.failed_because = Some("its start was refused".to_string());
    failed.runtime_crashes_in_a_row_implicating_it =
        RUNTIME_CRASHES_IN_A_ROW_THAT_FAIL_A_STREAM - 1;
    records_in(state_directory.path()).write(&failed).unwrap();
    let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));

    let streams_failed = runner.count_the_previous_runtime_runs_end_against_the_recorded_streams(
        &HowThePreviousRuntimeRunEnded::Crashed(CrashOfThePreviousRuntimeRun {
            causes_by_implicated_stream: [(refused.clone(), "SIGSEGV".to_string())].into(),
            ..CrashOfThePreviousRuntimeRun::default()
        }),
    );

    assert_eq!(streams_failed, vec![refused.clone()]);
    let recorded = the_record_of(state_directory.path(), &refused);
    assert!(
        recorded.is_failed_for_crashing_the_runtime(),
        "{recorded:?}"
    );
    assert!(
        recorded
            .failed_because
            .as_deref()
            .is_some_and(|failed_because| failed_because.contains("SIGSEGV")),
        "{recorded:?}"
    );
}

#[test]
#[serial]
fn a_recorded_stream_whose_venv_is_gone_is_held_stopped_failed_at_the_start_naming_its_interpreter()
{
    let state_directory = tempfile::tempdir().unwrap();
    let project = ProjectWithGraphDocuments::with_no_venv();
    project.write_the_graph_document(the_graph_of_a_stream_named("camera"));
    let camera = project.stream_id_of("camera");
    records_in(state_directory.path())
        .write(&a_record_of(&project, "camera"))
        .unwrap();
    let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));

    let reloads = runner.reload_every_recorded_stream_not_closed_at_once();

    assert!(
        matches!(
            reloads.as_slice(),
            [StreamReloadAtTheStart::HeldStopped { stream_id }] if *stream_id == camera
        ),
        "{reloads:?}"
    );
    let failed_because = the_record_of(state_directory.path(), &camera)
        .failed_because
        .expect("the stream is failed");
    assert!(
        failed_because.contains(&project.interpreter().display().to_string()),
        "{failed_because}"
    );
    assert!(failed_because.contains("re-load"), "{failed_because}");
    assert!(
        runner
            .loaded_stream_of_the_id(&camera)
            .unwrap()
            .is_held_stopped()
    );
    let fresh_runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
    assert!(matches!(
        fresh_runner
            .reload_every_recorded_stream_not_closed_at_once()
            .as_slice(),
        [StreamReloadAtTheStart::HeldStopped { .. }]
    ));
}

#[test]
#[serial]
fn the_reload_at_the_start_makes_a_gone_venv_once_for_every_record_of_its_project() {
    let state_directory = tempfile::tempdir().unwrap();
    let project = ProjectWithGraphDocuments::holding(the_graph_of_a_stream_named("camera"));
    project.hold_the_streams(&["camera", "microphone"], &[]);
    let records = records_in(state_directory.path());
    for stream_name in ["camera", "microphone"] {
        records.write(&a_record_of(&project, stream_name)).unwrap();
    }
    let stub_uv = StubRuntimeUnitUv::making_the_venv_moved_aside_from(&project);
    let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
    stub_uv.handed_to(&runner);

    let reloads = runner.reload_every_recorded_stream_not_closed_at_once();

    assert!(
        reloads
            .iter()
            .all(|reload| matches!(reload, StreamReloadAtTheStart::Reloaded { .. })),
        "{reloads:?}"
    );
    assert_eq!(reloads.len(), 2, "{reloads:?}");
    assert_eq!(stub_uv.runs(), ["sync"]);
}

/// A record of the stream `stream_name` of `project`, whose graph document it writes,
/// whose one node refuses an access nobody asked for unless its stream may ask the user.
fn a_record_of_a_stream_whose_node_asks_for_a_capture_device(
    project: &ProjectWithGraphDocuments,
    stream_name: &str,
) -> StreamRecord {
    project.write_the_graph_document(serde_json::json!({
        "stream": stream_name,
        "nodes": [{
            "name": "camera",
            "type": MockSourceRefusingAnAccessNobodyAskedFor::Node::node_class_import_path().as_str(),
            "config": {},
        }],
        "links": [],
    }));
    a_record_of(project, stream_name)
}

/// Mental revert: load the re-load's records as a start does, asking the
/// user, and the node sets up with its prompt up at login.
#[test]
#[serial]
fn the_reload_at_the_start_never_lets_a_node_ask_the_user_and_its_node_refuses_by_name() {
    let state_directory = tempfile::tempdir().unwrap();
    let project = ProjectWithGraphDocuments::with_no_venv().given_a_venv();
    let camera = project.stream_id_of("camera");
    records_in(state_directory.path())
        .write(&a_record_of_a_stream_whose_node_asks_for_a_capture_device(
            &project, "camera",
        ))
        .unwrap();
    let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));

    runner.reload_every_recorded_stream_not_closed_at_once();

    let reloaded = runner
        .loaded_stream_of_the_id_when_loaded(&camera)
        .expect("the rest of the stream re-loads");
    let refusal = reloaded
        .wait_until_every_node_is_running(STREAM_ACTION_TEST_DEADLINE)
        .expect_err("the node may not ask, so its setup refuses")
        .to_string();
    assert!(
        refusal.contains(MOCK_REFUSAL_OF_AN_ACCESS_NOBODY_ASKED_FOR),
        "{refusal}"
    );
    let listing = runner
        .list_streams()
        .into_iter()
        .find(|listing| listing.stream_id == camera)
        .expect("the re-loaded stream is listed");
    assert_eq!(
        listing.capture_devices_whose_access_was_never_asked_for,
        vec![PrivacyGatedCaptureDevice::Camera],
        "the listing names the device the stream waits on"
    );
}

#[test]
#[serial]
fn a_run_after_the_reload_lets_the_node_ask_the_user() {
    let state_directory = tempfile::tempdir().unwrap();
    let project = ProjectWithGraphDocuments::with_no_venv().given_a_venv();
    let camera = project.stream_id_of("camera");
    records_in(state_directory.path())
        .write(&a_record_of_a_stream_whose_node_asks_for_a_capture_device(
            &project, "camera",
        ))
        .unwrap();
    let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
    runner.reload_every_recorded_stream_not_closed_at_once();

    runner
        .stop_the_stream_finishing_its_pending_stop(&camera, None)
        .expect("the re-loaded stream stops");
    runner
        .load_the_project_off_its_queue(project.run_request())
        .expect("the stopped project runs");

    runner
        .loaded_stream_of_the_id_when_loaded(&camera)
        .expect("the started stream is loaded")
        .wait_until_every_node_is_running(STREAM_ACTION_TEST_DEADLINE)
        .expect("a start someone asked for may ask the user, so the node sets up");
    let listing = runner
        .list_streams()
        .into_iter()
        .find(|listing| listing.stream_id == camera)
        .expect("the started stream is listed");
    assert!(
        listing
            .capture_devices_whose_access_was_never_asked_for
            .is_empty(),
        "a started stream waits on no access: {listing:?}"
    );
}

#[test]
#[serial]
fn the_reload_at_the_start_holds_stopped_and_records_failed_in_uvs_words_a_stream_whose_venv_uv_cannot_make()
 {
    let state_directory = tempfile::tempdir().unwrap();
    let project = ProjectWithGraphDocuments::with_no_venv();
    project.write_the_graph_document(the_graph_of_a_stream_named("camera"));
    let camera = project.stream_id_of("camera");
    records_in(state_directory.path())
        .write(&a_record_of(&project, "camera"))
        .unwrap();
    let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
    let stub_uv = StubRuntimeUnitUv::failing_writing(UV_RESOLUTION_FAILURE);
    stub_uv.handed_to(&runner);

    let reloads = runner.reload_every_recorded_stream_not_closed_at_once();

    assert!(
        matches!(
            reloads.as_slice(),
            [StreamReloadAtTheStart::HeldStopped { stream_id }] if *stream_id == camera
        ),
        "{reloads:?}"
    );
    let failed_because = the_record_of(state_directory.path(), &camera)
        .failed_because
        .expect("the stream is failed");
    assert!(
        failed_because.contains(UV_RESOLUTION_FAILURE),
        "{failed_because}"
    );
    assert!(failed_because.contains("re-load"), "{failed_because}");
}
