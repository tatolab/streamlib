// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `tatolabd` hosting the machine's streams, GPU-free: the local API at its
//! fixed socket, the state directory and the runtime's own log, the kept
//! streams it re-loads at its start, and an interrupt's clean stop. A stream's
//! start creates the GPU context, so every stream here is refused at its start
//! — the Vulkan loader is left no driver — and the started stream is asserted
//! under `hardware-tests`. Built with
//! `--features machine-directories-under-a-test-root`.

#![cfg(feature = "machine-directories-under-a-test-root")]

mod common;

use common::{
    SpawnedTatolabd, TatolabTestMachineRoot, TemporaryRuntimeUnit, a_kept_stream_record,
    a_native_only_stream_graph, an_executable_standing_in_for_the_interpreter,
    call_a_tool_over_the_local_api, the_local_apis_answer_to_get, the_mode_of,
};

/// `STREAMLIB_RUNTIME_NAME` names the runtime until the machine is named, and
/// `graph` with no stream renders it over every loaded stream — none here.
#[test]
fn the_local_api_at_the_fixed_socket_answers_graph_with_the_runtime_name_and_no_streams() {
    let runtime_unit = TemporaryRuntimeUnit::with_its_lend();
    let machine_root = TatolabTestMachineRoot::new();
    let mut tatolabd_command = machine_root.tatolabd_command(&runtime_unit.tatolabd);
    tatolabd_command.env(
        "STREAMLIB_RUNTIME_NAME",
        "tatolabd-named-from-the-environment",
    );
    let mut runtime = SpawnedTatolabd::spawn_and_wait_until_serving(tatolabd_command);

    let graph = the_local_apis_answer_to_get(&machine_root.local_api_socket_path(), "/api/graph");

    assert_eq!(
        graph,
        serde_json::json!({
            "runtime_name": "tatolabd-named-from-the-environment",
            "streams": [],
        })
    );
    runtime.interrupt_and_expect_a_clean_exit();
}

#[test]
fn an_interrupt_stops_a_runtime_holding_no_stream_cleanly_and_removes_its_socket() {
    let runtime_unit = TemporaryRuntimeUnit::with_its_lend();
    let machine_root = TatolabTestMachineRoot::new();
    let mut runtime = SpawnedTatolabd::spawn_and_wait_until_serving(
        machine_root.tatolabd_command(&runtime_unit.tatolabd),
    );
    assert!(machine_root.local_api_socket_path().exists());

    runtime.interrupt_and_expect_a_clean_exit();

    assert!(
        !machine_root.local_api_socket_path().exists(),
        "{} was left behind:\n{}",
        machine_root.local_api_socket_path().display(),
        runtime.standard_error()
    );
}

/// The state directory and its kept-streams directory are the owner's alone,
/// and the records that belong to no stream land in the runtime's own log
/// beneath it.
#[test]
fn the_runtime_keeps_its_state_owner_only_and_writes_its_own_log_beneath_it() {
    let runtime_unit = TemporaryRuntimeUnit::with_its_lend();
    let machine_root = TatolabTestMachineRoot::new();
    let mut runtime = SpawnedTatolabd::spawn_and_wait_until_serving(
        machine_root.tatolabd_command(&runtime_unit.tatolabd),
    );
    runtime.interrupt_and_expect_a_clean_exit();

    assert_eq!(the_mode_of(&machine_root.state_directory()), 0o700);
    assert_eq!(the_mode_of(&machine_root.kept_streams_directory()), 0o700);
    let runtime_log_files: Vec<_> = std::fs::read_dir(machine_root.runtime_log_directory())
        .expect("the runtime's own log directory exists")
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.file_name()
                .and_then(|file_name| file_name.to_str())
                .is_some_and(|file_name| {
                    file_name.starts_with("tatolabd-") && file_name.ends_with(".jsonl")
                })
        })
        .collect();
    assert_eq!(runtime_log_files.len(), 1, "{runtime_log_files:?}");
    let runtime_log = std::fs::read_to_string(&runtime_log_files[0]).unwrap();
    assert!(
        runtime_log.contains("the runtime is serving:"),
        "{runtime_log}"
    );
}

/// At its start the runtime re-loads every kept stream neither stopped nor
/// failed, reporting each by name: a stopped one is left unloaded, and one
/// that cannot re-load — its interpreter gone, or its start refused — is
/// recorded failed for the reason, the runtime still up.
#[test]
fn every_kept_stream_not_stopped_is_re_loaded_at_the_start_and_one_that_cannot_is_failed_by_name() {
    let runtime_unit = TemporaryRuntimeUnit::with_its_lend();
    let machine_root = TatolabTestMachineRoot::new();
    let kept_stream_records = machine_root.kept_stream_records();
    for kept_stream_record in [
        a_kept_stream_record(
            "front",
            a_native_only_stream_graph("front"),
            &machine_root.project_directory(),
            &an_executable_standing_in_for_the_interpreter(),
            false,
        ),
        a_kept_stream_record(
            "parked",
            a_native_only_stream_graph("parked"),
            &machine_root.project_directory(),
            &an_executable_standing_in_for_the_interpreter(),
            true,
        ),
        a_kept_stream_record(
            "orphan",
            a_native_only_stream_graph("orphan"),
            &machine_root.project_directory(),
            &machine_root.project_directory().join(".venv/bin/python"),
            false,
        ),
    ] {
        kept_stream_records.write(&kept_stream_record).unwrap();
    }

    let mut runtime = SpawnedTatolabd::spawn_and_wait_until_serving(
        machine_root.tatolabd_command_with_no_vulkan_driver(&runtime_unit.tatolabd),
    );

    let standard_error = runtime.standard_error();
    assert!(
        standard_error.contains("[start] Starting the stream `front`"),
        "the kept stream `front` was loaded and its start begun:\n{standard_error}"
    );
    assert!(
        standard_error.contains("the kept stream `front` did not re-load, and is failed"),
        "{standard_error}"
    );
    assert!(
        standard_error.contains("the kept stream `parked` is stopped, so it is not re-loaded"),
        "{standard_error}"
    );
    assert!(
        !standard_error.contains("Starting the stream `parked`"),
        "{standard_error}"
    );
    assert!(
        standard_error.contains("the kept stream `orphan` did not re-load, and is failed"),
        "{standard_error}"
    );
    assert!(
        standard_error.contains(&format!(
            "its interpreter {} is gone",
            machine_root
                .project_directory()
                .join(".venv/bin/python")
                .display()
        )),
        "{standard_error}"
    );
    assert!(
        standard_error.contains("0 kept streams re-loaded, 2 skipped"),
        "{standard_error}"
    );

    let graph = the_local_apis_answer_to_get(&machine_root.local_api_socket_path(), "/api/graph");
    assert_eq!(graph["streams"], serde_json::json!([]), "{graph}");
    let listed = call_a_tool_over_the_local_api(
        &machine_root.local_api_socket_path(),
        "list_streams",
        serde_json::json!({}),
    )
    .json();
    let project_directory = machine_root.project_directory().display().to_string();
    let mut listed_streams = listed["streams"].as_array().unwrap().clone();
    listed_streams.sort_by_key(|listing| listing["name"].as_str().unwrap().to_owned());
    let failed_because: Vec<String> = listed_streams[..2]
        .iter()
        .map(|listing| {
            listing["failed_because"]
                .as_str()
                .unwrap_or_default()
                .to_owned()
        })
        .collect();
    assert!(
        failed_because[0].starts_with("it did not re-load at the runtime's start"),
        "{listed}"
    );
    assert!(
        failed_because[1].contains(&format!(
            "its interpreter {} is gone",
            machine_root
                .project_directory()
                .join(".venv/bin/python")
                .display()
        )),
        "{listed}"
    );
    assert_eq!(
        listed_streams,
        [
            serde_json::json!({"name": "front", "state": "failed", "project_directory": project_directory, "node_count": null, "failed_because": failed_because[0]}),
            serde_json::json!({"name": "orphan", "state": "failed", "project_directory": project_directory, "node_count": null, "failed_because": failed_because[1]}),
            serde_json::json!({"name": "parked", "state": "stopped", "project_directory": project_directory, "node_count": null, "failed_because": null}),
        ]
    );
    for stream_name in ["front", "parked", "orphan"] {
        assert!(
            kept_stream_records.read(stream_name).unwrap().is_some(),
            "the record of `{stream_name}` was kept"
        );
    }
    runtime.interrupt_and_expect_a_clean_exit();
}

/// A kept stream's record round-trips through the runtime: the owner's
/// ruling it records is written owner-only, read again by the next runtime
/// on the machine, and `remove_stream` forgets it.
#[test]
fn a_kept_streams_record_round_trips_through_a_restart_until_it_is_removed() {
    let runtime_unit = TemporaryRuntimeUnit::with_its_lend();
    let machine_root = TatolabTestMachineRoot::new();
    let kept_stream_records = machine_root.kept_stream_records();
    kept_stream_records
        .write(&a_kept_stream_record(
            "parked",
            a_native_only_stream_graph("parked"),
            &machine_root.project_directory(),
            &an_executable_standing_in_for_the_interpreter(),
            true,
        ))
        .unwrap();
    let record_path = kept_stream_records.record_path_of("parked").unwrap();

    let mut first_runtime = SpawnedTatolabd::spawn_and_wait_until_serving(
        machine_root.tatolabd_command(&runtime_unit.tatolabd),
    );
    let exposed = call_a_tool_over_the_local_api(
        &machine_root.local_api_socket_path(),
        "expose_port",
        serde_json::json!({"stream": "parked", "node": "testpattern", "port": "video", "level": "private"}),
    )
    .json();
    first_runtime.interrupt_and_expect_a_clean_exit();

    assert_eq!(
        exposed,
        serde_json::json!({"stream": "parked", "node": "testpattern", "port": "video", "level": "private", "recorded": true})
    );
    assert_eq!(the_mode_of(&record_path), 0o600);
    let recorded = kept_stream_records.read("parked").unwrap().unwrap();
    assert!(recorded.stopped);
    assert_eq!(
        serde_json::to_value(&recorded.exposure_rulings).unwrap(),
        serde_json::json!([{"node": "testpattern", "port": "video", "level": "private"}])
    );

    let mut second_runtime = SpawnedTatolabd::spawn_and_wait_until_serving(
        machine_root.tatolabd_command(&runtime_unit.tatolabd),
    );
    let listed = call_a_tool_over_the_local_api(
        &machine_root.local_api_socket_path(),
        "list_streams",
        serde_json::json!({}),
    )
    .json();
    let removed = call_a_tool_over_the_local_api(
        &machine_root.local_api_socket_path(),
        "remove_stream",
        serde_json::json!({"stream": "parked"}),
    )
    .json();
    second_runtime.interrupt_and_expect_a_clean_exit();

    assert_eq!(listed["streams"][0]["name"], "parked", "{listed}");
    assert_eq!(listed["streams"][0]["state"], "stopped", "{listed}");
    assert_eq!(
        removed,
        serde_json::json!({"stream": "parked", "unloaded": false, "forgotten": true})
    );
    assert!(
        !record_path.exists(),
        "{} was left behind",
        record_path.display()
    );
}
