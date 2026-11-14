// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use std::os::unix::fs::PermissionsExt;

/// A record of the stream `stream_name` in the project folder `folder`, its UUIDs derived
/// from both names so one pair always gives one record.
fn a_record_of(folder: &str, stream_name: &str) -> StreamRecord {
    let project_directory = PathBuf::from(format!("/home/someone/{folder}"));
    let project = uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_OID, folder.as_bytes());
    let location = StreamLocation {
        project_directory: project_directory.clone(),
        project,
        stream_asset: uuid::Uuid::new_v5(&project, stream_name.as_bytes()),
    };
    StreamRecord::of_a_running_stream(
        location.stream_id_named(folder, stream_name),
        location,
        &StreamEnvironment {
            interpreter: project_directory.join(".venv/bin/python"),
            project_directory,
        },
    )
}

fn current(record: StreamRecord) -> StreamRecordAsRead {
    StreamRecordAsRead::Current(record)
}

fn file_names_in(directory: &Path) -> Vec<String> {
    let mut file_names: Vec<String> = std::fs::read_dir(directory)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    file_names.sort();
    file_names
}

#[test]
fn a_written_record_reads_back_whole_from_a_file_named_after_its_stream_id() {
    let state_directory = tempfile::tempdir().unwrap();
    let records =
        StreamRecordsInTheStateDirectory::open(&state_directory.path().join("streams")).unwrap();
    let mut record = a_record_of("camera-app", "preview");
    record.stopped = true;
    let stream_id = record.stream_id.clone();

    records.write(&record).unwrap();

    assert_eq!(
        records.read(&stream_id).unwrap(),
        Some(current(record.clone()))
    );
    assert_eq!(records.read_every(), vec![Ok(current(record))]);
    assert_eq!(
        file_names_in(records.streams_directory()),
        [format!("{stream_id}.json")]
    );
}

#[test]
fn a_record_of_schema_8_or_earlier_is_read_as_an_earlier_runtimes() {
    for schema_version in [3, 4, 5, 6, 7, 8] {
        let state_directory = tempfile::tempdir().unwrap();
        let records = StreamRecordsInTheStateDirectory::open(state_directory.path()).unwrap();
        std::fs::write(
            records.streams_directory().join("hello-main-0a1b2c.json"),
            serde_json::to_vec(&serde_json::json!({
                "schema_version": schema_version,
                "stream_id": "hello-main-0a1b2c",
                "location": {
                    "project_directory": "/home/someone/hello",
                    "stream_function": "stream:main",
                },
                "display_name": "main",
                "interpreter": "/home/someone/hello/.venv/bin/python",
                "graph": {"stream": "main", "nodes": [], "links": []},
                "stopped": false,
                "failed_because": null,
                "runtime_crashes_in_a_row_implicating_it": 0,
                "pending_stream_actions": [],
            }))
            .unwrap(),
        )
        .unwrap();

        assert_eq!(
            records.read("hello-main-0a1b2c").unwrap(),
            Some(StreamRecordAsRead::RecordedByAnEarlierRuntime(
                StreamRecordOfAnEarlierRuntime {
                    stream_id: "hello-main-0a1b2c".to_string(),
                    schema_version,
                    project_directory: Some(PathBuf::from("/home/someone/hello")),
                }
            )),
            "schema {schema_version}"
        );
    }
}

#[test]
fn a_current_record_holding_a_graph_is_refused() {
    let state_directory = tempfile::tempdir().unwrap();
    let records = StreamRecordsInTheStateDirectory::open(state_directory.path()).unwrap();
    let record = a_record_of("camera-app", "preview");
    let mut written = serde_json::to_value(&record).unwrap();
    written["graph"] = serde_json::json!({"nodes": [], "links": []});
    std::fs::write(
        records.record_path_of(&record.stream_id).unwrap(),
        serde_json::to_vec(&written).unwrap(),
    )
    .unwrap();

    let refusal = records.read(&record.stream_id).unwrap_err().to_string();

    assert!(refusal.contains("graph"), "{refusal}");
}

#[test]
fn a_failed_record_reads_back_whole() {
    let state_directory = tempfile::tempdir().unwrap();
    let records = StreamRecordsInTheStateDirectory::open(state_directory.path()).unwrap();
    let mut record = a_record_of("camera-app", "preview");
    record.failed_because = Some("the graph document does not parse".to_string());

    records.write(&record).unwrap();

    assert_eq!(record.listing_state(), StreamListingState::Failed);
    assert_eq!(
        records.read(&record.stream_id).unwrap(),
        Some(current(record))
    );
}

#[test]
fn a_record_whose_id_begins_with_a_dot_is_read_like_any_other() {
    let state_directory = tempfile::tempdir().unwrap();
    let records = StreamRecordsInTheStateDirectory::open(state_directory.path()).unwrap();
    let record = a_record_of(".config-app", "preview");
    assert!(
        record.stream_id.starts_with(".config-app-"),
        "{}",
        record.stream_id
    );

    records.write(&record).unwrap();

    assert_eq!(records.read_every(), vec![Ok(current(record))]);
}

#[test]
fn a_record_holds_its_id_and_location_under_the_current_schema_and_no_display_name() {
    let record = a_record_of("camera-app", "preview");

    let written = serde_json::to_value(&record).unwrap();

    assert_eq!(written["schema_version"], STREAM_RECORD_SCHEMA_VERSION);
    assert_eq!(written["pending_stream_actions"], serde_json::json!([]));
    assert_eq!(written["stream_id"], record.stream_id);
    assert_eq!(
        written["location"],
        serde_json::json!({
            "project_directory": "/home/someone/camera-app",
            "project": record.location.project.to_string(),
            "stream_asset": record.location.stream_asset.to_string(),
        })
    );
    assert_eq!(written.get("graph"), None);
    assert_eq!(written.get("display_name"), None);
}

#[test]
fn the_streams_directory_is_owner_only_and_each_record_is_its_owners_alone() {
    let state_directory = tempfile::tempdir().unwrap();
    let streams_directory = state_directory.path().join("streams");
    let records = StreamRecordsInTheStateDirectory::open(&streams_directory).unwrap();
    let record = a_record_of("camera-app", "preview");

    records.write(&record).unwrap();

    let mode_of = |path: &Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode_of(&streams_directory), 0o700);
    assert_eq!(
        mode_of(&records.record_path_of(&record.stream_id).unwrap()),
        0o600
    );
}

#[test]
fn a_rewrite_replaces_the_record_whole_and_leaves_no_temporary_file() {
    let state_directory = tempfile::tempdir().unwrap();
    let records = StreamRecordsInTheStateDirectory::open(state_directory.path()).unwrap();
    let first = a_record_of("camera-app", "preview");
    records.write(&first).unwrap();

    let mut stopped = first.clone();
    stopped.stopped = true;
    records.write(&stopped).unwrap();

    assert_eq!(
        records.read(&first.stream_id).unwrap(),
        Some(current(stopped))
    );
    assert_eq!(
        file_names_in(records.streams_directory()),
        [format!("{}.json", first.stream_id)]
    );
}

#[test]
fn every_record_is_read_and_each_unreadable_one_is_reported_by_its_path() {
    let state_directory = tempfile::tempdir().unwrap();
    let records = StreamRecordsInTheStateDirectory::open(state_directory.path()).unwrap();
    let directory = records.streams_directory().to_path_buf();
    let camera = a_record_of("a-camera", "main");
    records.write(&camera).unwrap();
    std::fs::write(directory.join("b-malformed.json"), b"{ not json").unwrap();
    let mut later_schema = serde_json::to_value(a_record_of("c-later", "main")).unwrap();
    later_schema["schema_version"] = serde_json::json!(STREAM_RECORD_SCHEMA_VERSION + 1);
    later_schema["crashed_at"] = serde_json::json!("2026-10-10T00:00:00Z");
    std::fs::write(
        directory.join("c-later.json"),
        serde_json::to_vec(&later_schema).unwrap(),
    )
    .unwrap();
    std::fs::write(directory.join("notes.txt"), b"not a record").unwrap();
    std::fs::write(directory.join(".e-half.json.1.0.tmp"), b"{").unwrap();

    let every_record = records.read_every();

    assert_eq!(every_record.len(), 3, "{every_record:?}");
    assert_eq!(every_record[0], Ok(current(camera)));
    let malformed = every_record[1].as_ref().unwrap_err();
    assert_eq!(malformed.path, directory.join("b-malformed.json"));
    assert!(malformed.reason.contains("not JSON"), "{malformed}");
    let later = every_record[2].as_ref().unwrap_err();
    assert_eq!(later.path, directory.join("c-later.json"));
    assert!(
        later.reason.contains(&format!(
            "schema version {}",
            STREAM_RECORD_SCHEMA_VERSION + 1
        )),
        "{later}"
    );
}

#[test]
fn updates_from_many_threads_each_keep_what_the_others_changed() {
    let state_directory = tempfile::tempdir().unwrap();
    let records = StreamRecordsInTheStateDirectory::open(state_directory.path()).unwrap();
    let record = a_record_of("camera-app", "preview");
    records.write(&record).unwrap();

    std::thread::scope(|scope| {
        for action_number in 0..16 {
            let records = records.clone();
            let stream_id = record.stream_id.clone();
            scope.spawn(move || {
                records
                    .change_the_record(&stream_id, |recorded| {
                        recorded.queue_the_pending_stream_action(
                            &format!("R-{action_number}"),
                            PendingStreamActionKind::Stop,
                        );
                    })
                    .unwrap();
            });
        }
    });

    let Some(StreamRecordAsRead::Current(updated)) = records.read(&record.stream_id).unwrap()
    else {
        panic!("the record is gone");
    };
    assert_eq!(updated.pending_stream_actions.len(), 16);
}

#[test]
fn a_loads_record_keeps_the_pending_actions_queued_while_it_loaded() {
    let state_directory = tempfile::tempdir().unwrap();
    let records = StreamRecordsInTheStateDirectory::open(state_directory.path()).unwrap();
    let mut renamed_while_it_loaded = a_record_of("camera-app", "preview");
    renamed_while_it_loaded.queue_the_pending_stream_action("R-3", PendingStreamActionKind::Stop);
    records.write(&renamed_while_it_loaded).unwrap();
    let mut what_the_load_made = a_record_of("camera-app", "preview");
    what_the_load_made.failed_because = Some("it raised".to_string());

    let written = records
        .write_the_record_a_load_made(what_the_load_made)
        .unwrap();

    assert_eq!(written.failed_because.as_deref(), Some("it raised"));
    assert_eq!(
        written.pending_stream_actions,
        renamed_while_it_loaded.pending_stream_actions
    );
    assert_eq!(
        records.read(&written.stream_id).unwrap(),
        Some(current(written))
    );
}

#[test]
fn a_loads_record_keeps_the_pause_unless_the_load_holds_the_stream_stopped() {
    let state_directory = tempfile::tempdir().unwrap();
    let records = StreamRecordsInTheStateDirectory::open(state_directory.path()).unwrap();
    let mut paused = a_record_of("camera-app", "preview");
    paused.paused = true;
    records.write(&paused).unwrap();

    let reloaded_playing = records
        .write_the_record_a_load_made(a_record_of("camera-app", "preview"))
        .unwrap();
    assert!(reloaded_playing.comes_back_paused_at_a_restart());

    let mut held_stopped = a_record_of("camera-app", "preview");
    held_stopped.mark_open_with_its_playback_stopped();
    let reloaded_stopped = records.write_the_record_a_load_made(held_stopped).unwrap();
    assert!(!reloaded_stopped.paused);
    assert!(!reloaded_stopped.comes_back_paused_at_a_restart());
}

#[test]
fn a_record_written_without_a_pause_reads_back_playing() {
    let state_directory = tempfile::tempdir().unwrap();
    let records = StreamRecordsInTheStateDirectory::open(state_directory.path()).unwrap();
    let record = a_record_of("camera-app", "preview");
    let mut written_without_a_pause = serde_json::to_value(&record).unwrap();
    written_without_a_pause
        .as_object_mut()
        .unwrap()
        .remove("paused");
    std::fs::write(
        records.record_path_of(&record.stream_id).unwrap(),
        serde_json::to_vec(&written_without_a_pause).unwrap(),
    )
    .unwrap();

    assert_eq!(
        records.read(&record.stream_id).unwrap(),
        Some(current(record))
    );
}

#[test]
fn a_pending_stop_records_a_record_stopped_until_its_action_finishes_it() {
    let mut record = a_record_of("camera-app", "preview");
    assert!(!record.is_stopped_once_its_pending_stream_actions_run());

    record.queue_the_pending_stream_action("R-1", PendingStreamActionKind::Stop);
    assert!(record.is_stopped_once_its_pending_stream_actions_run());
    record.finish_the_pending_stream_action("R-1");
    assert!(!record.is_stopped_once_its_pending_stream_actions_run());
    assert!(record.pending_stream_actions.is_empty());
}

#[test]
fn a_schema_2_record_is_read_as_an_earlier_runtimes_keyed_by_its_file_name() {
    let state_directory = tempfile::tempdir().unwrap();
    let records = StreamRecordsInTheStateDirectory::open(state_directory.path()).unwrap();
    std::fs::write(
        records.streams_directory().join("main.json"),
        serde_json::to_vec(&serde_json::json!({
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
        .unwrap(),
    )
    .unwrap();

    let earlier = StreamRecordOfAnEarlierRuntime {
        stream_id: "main".to_string(),
        schema_version: 2,
        project_directory: Some(PathBuf::from("/home/someone/hello")),
    };
    assert_eq!(
        records.read_every(),
        vec![Ok(StreamRecordAsRead::RecordedByAnEarlierRuntime(
            earlier.clone()
        ))]
    );
    assert_eq!(
        records.read("main").unwrap(),
        Some(StreamRecordAsRead::RecordedByAnEarlierRuntime(
            earlier.clone()
        ))
    );
    let failed_because = earlier.failed_because();
    assert!(
        failed_because.contains("an earlier runtime recorded it"),
        "{failed_because}"
    );
    assert!(
        failed_because.contains("run it again with `tatolab open --play` in /home/someone/hello"),
        "{failed_because}"
    );
    assert!(records.remove("main").unwrap());
    assert!(records.read_every().is_empty());
}

#[test]
fn a_record_whose_stream_id_is_not_its_file_name_is_reported_unreadable_by_its_path() {
    let state_directory = tempfile::tempdir().unwrap();
    let records = StreamRecordsInTheStateDirectory::open(state_directory.path()).unwrap();
    let camera = a_record_of("a-camera", "main");
    records.write(&camera).unwrap();
    let renamed_copy = records.streams_directory().join("zz-copy.json");
    std::fs::copy(
        records.record_path_of(&camera.stream_id).unwrap(),
        &renamed_copy,
    )
    .unwrap();

    let every_record = records.read_every();

    assert_eq!(every_record.len(), 2, "{every_record:?}");
    assert_eq!(every_record[0], Ok(current(camera.clone())));
    let mismatched = every_record[1].as_ref().unwrap_err();
    assert_eq!(mismatched.path, renamed_copy);
    assert!(
        mismatched
            .reason
            .contains(&format!("`{}`", camera.stream_id)),
        "{mismatched}"
    );
    let refusal = records.read("zz-copy").unwrap_err();
    assert_eq!(refusal.path, renamed_copy);
}

#[test]
fn a_record_carrying_a_field_this_runtime_does_not_read_is_reported_naming_it() {
    let state_directory = tempfile::tempdir().unwrap();
    let records = StreamRecordsInTheStateDirectory::open(state_directory.path()).unwrap();
    let record = a_record_of("camera-app", "preview");
    let mut record_document = serde_json::to_value(&record).unwrap();
    record_document["stoped"] = serde_json::json!(true);
    std::fs::write(
        records.record_path_of(&record.stream_id).unwrap(),
        serde_json::to_vec(&record_document).unwrap(),
    )
    .unwrap();

    let refusal = records.read(&record.stream_id).unwrap_err().to_string();

    assert!(
        refusal.contains(&format!("{}.json", record.stream_id)),
        "{refusal}"
    );
    assert!(refusal.contains("stoped"), "{refusal}");
}

#[test]
fn a_removed_record_is_gone_and_removing_it_again_reports_there_was_none() {
    let state_directory = tempfile::tempdir().unwrap();
    let records = StreamRecordsInTheStateDirectory::open(state_directory.path()).unwrap();
    let record = a_record_of("camera-app", "preview");
    records.write(&record).unwrap();

    assert!(records.remove(&record.stream_id).unwrap());
    assert_eq!(records.read(&record.stream_id).unwrap(), None);
    assert!(!records.remove(&record.stream_id).unwrap());
    assert!(records.read_every().is_empty());
}

#[test]
fn a_name_that_is_not_a_stream_id_names_no_record() {
    let state_directory = tempfile::tempdir().unwrap();
    let records = StreamRecordsInTheStateDirectory::open(state_directory.path()).unwrap();

    assert_eq!(records.record_path_of("../escape"), None);
    assert_eq!(records.read("Camera Preview").unwrap(), None);
    assert!(!records.remove("Camera Preview").unwrap());
}

#[test]
fn a_streams_path_that_is_not_a_directory_is_refused_by_name() {
    let state_directory = tempfile::tempdir().unwrap();
    let not_a_directory = state_directory.path().join("streams");
    std::fs::write(&not_a_directory, b"").unwrap();

    let refusal = StreamRecordsInTheStateDirectory::open(&not_a_directory)
        .unwrap_err()
        .to_string();

    assert!(
        refusal.contains(&not_a_directory.display().to_string()),
        "{refusal}"
    );
}
