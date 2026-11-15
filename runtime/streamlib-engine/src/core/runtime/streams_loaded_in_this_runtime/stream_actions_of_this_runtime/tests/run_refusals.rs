// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

#[test]
#[serial]
fn a_run_on_a_runtime_given_no_state_directory_is_refused_by_name() {
    let runner = a_runner_recording_its_streams_in(None);
    let project = ProjectWithGraphDocuments::holding(the_graph_of_a_stream_named("camera"));

    let refusal = refusal_of(runner.load_the_project_off_its_queue(project.run_request()));

    assert!(refusal.contains("records no stream"), "{refusal}");
    assert!(refusal.contains("state directory"), "{refusal}");
    assert!(runner.ids_of_the_loaded_streams().is_empty());
}

#[test]
#[serial]
fn a_second_streams_directory_is_refused_naming_the_first() {
    let state_directory = tempfile::tempdir().unwrap();
    let first = state_directory.path().join("streams");
    let runner = a_runner_recording_its_streams_in(Some(&first));

    let refusal = refusal_of(
        runner.record_streams_in_the_state_directory(&state_directory.path().join("other")),
    );

    assert!(refusal.contains(&first.display().to_string()), "{refusal}");
}

#[test]
#[serial]
fn a_run_whose_id_a_loaded_stream_of_another_location_holds_is_refused_naming_both() {
    let state_directory = tempfile::tempdir().unwrap();
    let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
    let (project, other_project) =
        ProjectWithGraphDocuments::two_with_no_venv_whose_ids_collide("camera");
    let project = project.given_a_venv();
    project.write_the_graph_document(the_graph_of_a_stream_named("camera"));
    let camera = project.stream_id_of("camera");
    let other_location = other_project.location_of("camera");
    assert_eq!(other_project.stream_id_of("camera"), camera);
    let holder =
        a_stream_loaded_from_an_unwritten_record(&runner, &a_record_of(&other_project, "camera"));

    let refusal = refusal_of(runner.load_the_project_off_its_queue(project.run_request()));

    assert!(refusal.contains(&format!("`{camera}`")), "{refusal}");
    assert!(
        refusal.contains(&project.location_of("camera").to_string()),
        "{refusal}"
    );
    assert!(refusal.contains(&other_location.to_string()), "{refusal}");
    assert!(refusal.contains("cannot share one id"), "{refusal}");
    assert_eq!(
        runner
            .loaded_stream_of_the_id(&camera)
            .unwrap()
            .stream_tag(),
        holder.stream_tag()
    );
    assert!(!holder.has_ended());
    assert!(records_in(state_directory.path()).read_every().is_empty());
}

#[test]
#[serial]
fn a_run_whose_id_a_stream_its_host_loaded_holds_is_refused_naming_both() {
    let state_directory = tempfile::tempdir().unwrap();
    let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
    let project = ProjectWithGraphDocuments::holding(the_graph_of_a_stream_named("camera"));
    let camera = project.stream_id_of("camera");
    let host_project = ProjectWithGraphDocuments::with_no_venv();
    let graph = GraphSnapshot::from_graph_document(the_graph_of_a_stream_named("camera")).unwrap();
    let host_loaded = runner
        .load_stream_from_graph_snapshot(
            &graph,
            OptionsForLoadingOneStream::at_stream_location(
                camera.clone(),
                host_project.location_of("camera"),
            ),
        )
        .expect("the host loads its stream under the id it names");

    let refusal = refusal_of(runner.load_the_project_off_its_queue(project.run_request()));

    assert!(refusal.contains("the loaded stream from"), "{refusal}");
    assert!(
        refusal.contains(&host_project.path().display().to_string()),
        "{refusal}"
    );
    assert!(
        refusal.contains(&project.location_of("camera").to_string()),
        "{refusal}"
    );
    assert!(!host_loaded.has_ended());
}

#[test]
#[serial]
fn a_run_whose_id_a_record_of_another_location_holds_is_refused_naming_both_whatever_its_state() {
    let (project, other_project) =
        ProjectWithGraphDocuments::two_with_no_venv_whose_ids_collide("camera");
    let project = project.given_a_venv();
    project.write_the_graph_document(the_graph_of_a_stream_named("camera"));
    let camera = project.stream_id_of("camera");
    let other_location = other_project.location_of("camera");
    assert_eq!(other_project.stream_id_of("camera"), camera);

    let holder_states: [(&str, fn(&mut StreamRecord)); 3] = [
        ("stopped", |record| record.stopped = true),
        ("failed", |record| {
            record.failed_because = Some("it crashed the runtime".to_string())
        }),
        ("running", |_| {}),
    ];
    for (state, mark) in holder_states {
        let state_directory = tempfile::tempdir().unwrap();
        let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
        let mut holder = a_record_of(&other_project, "camera");
        mark(&mut holder);
        records_in(state_directory.path()).write(&holder).unwrap();

        let refusal = refusal_of(runner.load_the_project_off_its_queue(project.run_request()));

        assert!(
            refusal.contains(&other_location.to_string()),
            "{state}: {refusal}"
        );
        assert!(
            refusal.contains(&project.location_of("camera").to_string()),
            "{state}: {refusal}"
        );
        assert!(
            refusal.contains("cannot share one id"),
            "{state}: {refusal}"
        );
        if state != "running" {
            assert!(refusal.contains(state), "{state}: {refusal}");
        }
        assert!(runner.ids_of_the_loaded_streams().is_empty());
        assert_eq!(
            the_current_record_in(state_directory.path(), &camera),
            Some(holder)
        );
        if state == "failed" {
            let refusal = refusal_of(runner.remove_one_stream("nothing-here"));
            assert!(refusal.contains(&format!("{camera} (failed)")), "{refusal}");
        }
    }
}

#[test]
#[serial]
fn a_run_whose_project_file_does_not_read_is_refused_leaving_the_running_stream_and_its_record() {
    let state_directory = tempfile::tempdir().unwrap();
    let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
    let project = ProjectWithGraphDocuments::holding(the_graph_of_a_stream_named("camera"));
    let camera = project.stream_id_of("camera");
    let record = a_record_of(&project, "camera");
    let running =
        a_recorded_stream_loaded_without_its_start(&runner, state_directory.path(), &record);
    std::fs::write(project.path().join(PROJECT_FILE_NAME), "{").unwrap();

    let refusal = refusal_of(runner.load_the_project_off_its_queue(project.run_request()));

    assert!(refusal.contains("was not loaded"), "{refusal}");
    assert!(refusal.contains(PROJECT_FILE_NAME), "{refusal}");
    assert_eq!(
        runner
            .loaded_stream_of_the_id(&camera)
            .unwrap()
            .stream_tag(),
        running.stream_tag(),
        "the running stream is the one loaded before the replace"
    );
    assert!(!running.has_ended());
    assert_eq!(
        the_current_record_in(state_directory.path(), &camera),
        Some(record)
    );
}

#[test]
#[serial]
fn a_run_of_a_directory_holding_no_project_file_is_refused_naming_it() {
    let state_directory = tempfile::tempdir().unwrap();
    let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
    let project = ProjectWithGraphDocuments::holding(the_graph_of_a_stream_named("camera"));
    std::fs::remove_file(project.path().join(PROJECT_FILE_NAME)).unwrap();

    let refusal = refusal_of(runner.load_the_project_off_its_queue(project.run_request()));

    assert!(refusal.contains(PROJECT_FILE_NAME), "{refusal}");
    assert!(runner.ids_of_the_loaded_streams().is_empty());
    assert!(records_in(state_directory.path()).read_every().is_empty());
}

#[test]
#[serial]
fn a_run_of_every_stream_one_of_whose_ids_the_host_holds_is_refused_loading_none() {
    let state_directory = tempfile::tempdir().unwrap();
    let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
    let project = ProjectWithGraphDocuments::holding(the_graph_of_a_stream_named("camera"));
    let hosted = a_stream_its_host_loaded(&runner, &project, "preview");
    project.hold_the_streams(&["camera", "preview"], &[]);

    let refusal = refusal_of(runner.load_the_project_off_its_queue(project.run_request()));

    assert!(
        refusal.contains(&project.stream_id_of("preview")),
        "{refusal}"
    );
    assert_eq!(runner.ids_of_the_loaded_streams(), [hosted.stream_id()]);
    assert!(records_in(state_directory.path()).read_every().is_empty());
}

#[test]
#[serial]
fn a_second_copy_of_a_project_is_refused_while_the_first_still_holds_it() {
    let state_directory = tempfile::tempdir().unwrap();
    let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
    let project = ProjectWithGraphDocuments::holding(the_graph_of_a_stream_named("camera"));
    let camera = project.stream_id_of("camera");
    runner
        .load_the_project_off_its_queue(project.run_request())
        .unwrap();
    let copy = project.another_copy();
    copy.write_the_graph_document(the_graph_of_a_stream_named("camera"));

    let refusal = refusal_of(runner.load_the_project_off_its_queue(copy.run_request()));

    assert!(refusal.contains("cannot share one id"), "{refusal}");
    assert!(refusal.contains("its own project UUID"), "{refusal}");
    assert_eq!(
        runner.ids_of_the_loaded_streams(),
        std::slice::from_ref(&camera)
    );
    assert_eq!(
        the_record_of(state_directory.path(), &camera)
            .location
            .project_directory,
        project.path()
    );
}

#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
#[serial]
fn a_run_whose_record_is_not_written_unloads_its_stream_and_records_nothing() {
    let state_directory = tempfile::tempdir().unwrap();
    let streams_directory = state_directory.path().join("streams");
    let runner = a_runner_recording_its_streams_in(Some(&streams_directory));
    let project = ProjectWithGraphDocuments::holding(the_graph_of_a_stream_named("camera"));

    let refusal = {
        let _refusing_writes = StreamsDirectoryRefusingWrites::from_now_on(&streams_directory);
        refusal_of(runner.load_the_project_off_its_queue(project.run_request()))
    };

    assert!(refusal.contains("was not written"), "{refusal}");
    assert!(runner.ids_of_the_loaded_streams().is_empty());
    assert!(records_in(&streams_directory).read_every().is_empty());
}

#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1"
)]
#[test]
#[serial]
fn a_replace_whose_record_is_not_written_leaves_the_running_stream_as_it_was() {
    let state_directory = tempfile::tempdir().unwrap();
    let streams_directory = state_directory.path().join("streams");
    let runner = a_runner_recording_its_streams_in(Some(&streams_directory));
    let project = ProjectWithGraphDocuments::holding(the_graph_of_a_stream_named("camera"));
    let camera = project.stream_id_of("camera");
    let first_run = runner
        .load_the_project_off_its_queue(project.run_request())
        .unwrap();
    let (_, first_stream_tag, _) = the_one_running_stream_of(&first_run);
    let mut previous_record = the_record_of(&streams_directory, &camera);
    previous_record.runtime_crashes_in_a_row_implicating_it = 1;
    records_in(&streams_directory)
        .write(&previous_record)
        .unwrap();
    let mut a_replacement_graph = the_graph_of_a_stream_named("camera");
    a_replacement_graph["nodes"][0]["name"] = "replacement-source".into();
    project.write_the_graph_document(a_replacement_graph);

    let refusal = {
        let _refusing_writes = StreamsDirectoryRefusingWrites::from_now_on(&streams_directory);
        refusal_of(runner.load_the_project_off_its_queue(project.run_request()))
    };

    assert!(refusal.contains("was not written"), "{refusal}");
    let running = runner
        .loaded_stream_of_the_id(&camera)
        .expect("the running stream stays loaded");
    assert_eq!(
        running.stream_tag(),
        first_stream_tag,
        "the stream running is the previous record's, not the one whose record was refused"
    );
    assert_eq!(
        the_current_record_in(&streams_directory, &camera),
        Some(previous_record)
    );
}

#[test]
#[serial]
fn a_run_in_a_missing_project_directory_is_refused_with_nothing_recorded() {
    let state_directory = tempfile::tempdir().unwrap();
    let runner = a_runner_recording_its_streams_in(Some(state_directory.path()));
    let project = ProjectWithGraphDocuments::with_no_venv();
    let stub_uv = StubRuntimeUnitUv::failing_writing(UV_RESOLUTION_FAILURE);
    stub_uv.handed_to(&runner);

    let refusal = refusal_of(
        runner.load_the_project_off_its_queue(ProjectStreamsToReadAndLoad {
            project_directory: project.path().join("missing"),
            ..project.run_request()
        }),
    );

    assert!(refusal.contains("holds no project"), "{refusal}");
    assert!(stub_uv.runs().is_empty(), "{:?}", stub_uv.runs());
    assert!(records_in(state_directory.path()).read_every().is_empty());
}
