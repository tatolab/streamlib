// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `tatolabd` hosting the machine's streams, GPU-free: the local API at its
//! fixed socket, the state directory and the runtime's own log, the recorded
//! streams it re-loads at its start, and an interrupt's clean stop. A stream's
//! start creates the GPU context, so every stream here is refused at its start
//! — the Vulkan loader is left no driver — and the started stream is asserted
//! under `hardware-tests`. Built with
//! `--features machine-directories-under-a-test-root`.

#![cfg(feature = "machine-directories-under-a-test-root")]

mod common;

use common::{
    SpawnedTatolabd, TatolabTestMachineRoot, TemporaryRuntimeUnit, a_native_only_stream_graph,
    a_stream_record, an_executable_standing_in_for_the_interpreter, call_a_tool_over_the_local_api,
    the_local_apis_answer_to_get, the_mode_of,
};

/// This machine's hostname: the kernel's UTS node name, which `gethostname`
/// returns on Linux.
#[cfg(target_os = "linux")]
fn this_hosts_name() -> String {
    std::fs::read_to_string("/proc/sys/kernel/hostname")
        .expect("the test host reports a hostname")
        .trim_end_matches('\n')
        .to_string()
}

/// `graph` with no stream names the machine by its hostname, cast, over every
/// loaded stream — none here.
#[cfg(target_os = "linux")]
#[test]
fn graph_answers_the_machine_by_its_hostname_cast_and_no_streams() {
    let runtime_unit = TemporaryRuntimeUnit::with_its_lend();
    let machine_root = TatolabTestMachineRoot::new();
    let the_hostname_cast =
        streamlib::sdk::runtime::MachineName::from_a_hostname(&this_hosts_name())
            .expect("the test host's hostname names something")
            .name()
            .to_string();

    let mut runtime = SpawnedTatolabd::spawn_and_wait_until_serving(
        machine_root.tatolabd_command(&runtime_unit.tatolabd),
    );
    let graph = the_local_apis_answer_to_get(&machine_root.local_api_socket_path(), "/api/graph");
    runtime.interrupt_and_expect_a_clean_exit();

    assert_eq!(
        graph,
        serde_json::json!({ "machine": the_hostname_cast, "streams": [] })
    );
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

/// The state directory and its streams directory are the owner's alone,
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
    assert_eq!(the_mode_of(&machine_root.streams_directory()), 0o700);
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

/// At its start the runtime re-loads every recorded stream neither stopped nor
/// failed, reporting each by its id: a stopped one is left unloaded, one whose
/// interpreter is gone is recorded failed for it, one the engine could not
/// start — no GPU — is re-loaded at the next start, and a record an earlier
/// runtime wrote is listed failed, keyed by its file name, the runtime still up.
#[test]
fn every_recorded_stream_not_stopped_is_re_loaded_at_the_start_and_one_that_cannot_is_named() {
    let runtime_unit = TemporaryRuntimeUnit::with_its_lend();
    let machine_root = TatolabTestMachineRoot::new();
    let stream_records = machine_root.stream_records();
    let [front, parked, orphan] = [
        a_stream_record(
            "front",
            a_native_only_stream_graph("front"),
            &machine_root.project_directory(),
            &an_executable_standing_in_for_the_interpreter(),
            false,
        ),
        a_stream_record(
            "parked",
            a_native_only_stream_graph("parked"),
            &machine_root.project_directory(),
            &an_executable_standing_in_for_the_interpreter(),
            true,
        ),
        a_stream_record(
            "orphan",
            a_native_only_stream_graph("orphan"),
            &machine_root.project_directory(),
            &machine_root.project_directory().join(".venv/bin/python"),
            false,
        ),
    ];
    for record in [&front, &parked, &orphan] {
        stream_records.write(record).unwrap();
        assert!(
            machine_root
                .streams_directory()
                .join(format!("{}.json", record.stream_id))
                .is_file(),
            "the record of `{}` is `streams/<stream id>.json`",
            record.stream_id
        );
    }
    std::fs::write(
        machine_root.streams_directory().join("main.json"),
        serde_json::json!({
            "schema_version": 2,
            "stream_name": "main",
            "project_directory": machine_root.project_directory(),
            "interpreter": an_executable_standing_in_for_the_interpreter(),
            "stream_function": null,
            "graph": a_native_only_stream_graph("main"),
            "stopped": false,
            "failed_because": null,
            "runtime_crashes_in_a_row_implicating_it": 0,
            "exposure_rulings": [],
        })
        .to_string(),
    )
    .unwrap();

    let mut runtime = SpawnedTatolabd::spawn_and_wait_until_serving(
        machine_root.tatolabd_command_with_no_vulkan_driver(&runtime_unit.tatolabd),
    );

    let standard_error = runtime.standard_error();
    assert!(
        standard_error.contains(&format!(
            "[start] Starting the stream `{}`",
            front.stream_id
        )),
        "the recorded stream `{}` was loaded and its start begun:\n{standard_error}",
        front.stream_id
    );
    assert!(
        standard_error.contains(&format!(
            "the stream `{}` was not re-loaded, and is re-loaded at the runtime's next start",
            front.stream_id
        )),
        "{standard_error}"
    );
    assert!(
        standard_error.contains(&format!(
            "the stream `{}` is stopped, so it is not re-loaded",
            parked.stream_id
        )),
        "{standard_error}"
    );
    assert!(
        !standard_error.contains(&format!("Starting the stream `{}`", parked.stream_id)),
        "{standard_error}"
    );
    assert!(
        standard_error.contains(&format!(
            "the stream `{}` did not re-load, and is failed",
            orphan.stream_id
        )),
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
        standard_error.contains("the stream `main` is failed, so it is not re-loaded"),
        "{standard_error}"
    );
    assert!(
        standard_error.contains("0 recorded streams re-loaded, 3 skipped"),
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
    let listed_streams = listed["streams"].as_array().unwrap().clone();
    let listing_of = |stream_id: &str| {
        listed_streams
            .iter()
            .find(|listing| listing["stream"] == stream_id)
            .unwrap_or_else(|| panic!("`{stream_id}` is listed: {listed}"))
            .clone()
    };
    let location_of = |function: &str| {
        serde_json::json!({
            "project_directory": machine_root.project_directory(),
            "stream_function": format!("stream:{function}"),
        })
    };
    assert_eq!(listed_streams.len(), 4, "{listed}");
    assert_eq!(
        listing_of(&front.stream_id),
        serde_json::json!({"stream": front.stream_id, "display_name": "front", "location": location_of("front"), "state": "running", "node_count": null, "failed_because": null})
    );
    assert_eq!(
        listing_of(&parked.stream_id),
        serde_json::json!({"stream": parked.stream_id, "display_name": "parked", "location": location_of("parked"), "state": "stopped", "node_count": null, "failed_because": null})
    );
    let orphan_listing = listing_of(&orphan.stream_id);
    assert_eq!(orphan_listing["state"], "failed", "{listed}");
    assert!(
        orphan_listing["failed_because"]
            .as_str()
            .unwrap_or_default()
            .contains(&format!(
                "its interpreter {} is gone",
                machine_root
                    .project_directory()
                    .join(".venv/bin/python")
                    .display()
            )),
        "{listed}"
    );
    let earlier_runtimes_listing = listing_of("main");
    assert_eq!(earlier_runtimes_listing["state"], "failed", "{listed}");
    assert_eq!(
        earlier_runtimes_listing["location"],
        serde_json::Value::Null
    );
    assert!(
        earlier_runtimes_listing["failed_because"]
            .as_str()
            .unwrap_or_default()
            .contains("an earlier runtime recorded it"),
        "{listed}"
    );
    for record in [&front, &parked, &orphan] {
        assert!(
            machine_root
                .current_stream_record_of(&record.stream_id)
                .is_some(),
            "the record of `{}` stayed",
            record.stream_id
        );
    }
    runtime.interrupt_and_expect_a_clean_exit();
}

/// A recorded stream's record round-trips through the runtime: the owner's
/// ruling it records is written owner-only, read again by the next runtime
/// on the machine, and `remove_stream` forgets it.
#[test]
fn a_streams_record_round_trips_through_a_restart_until_it_is_removed() {
    let runtime_unit = TemporaryRuntimeUnit::with_its_lend();
    let machine_root = TatolabTestMachineRoot::new();
    let stream_records = machine_root.stream_records();
    let parked = a_stream_record(
        "parked",
        a_native_only_stream_graph("parked"),
        &machine_root.project_directory(),
        &an_executable_standing_in_for_the_interpreter(),
        true,
    );
    stream_records.write(&parked).unwrap();
    let parked_stream_id = parked.stream_id;
    let record_path = stream_records.record_path_of(&parked_stream_id).unwrap();
    assert_eq!(
        record_path,
        machine_root
            .streams_directory()
            .join(format!("{parked_stream_id}.json"))
    );

    let mut first_runtime = SpawnedTatolabd::spawn_and_wait_until_serving(
        machine_root.tatolabd_command(&runtime_unit.tatolabd),
    );
    let exposed = call_a_tool_over_the_local_api(
        &machine_root.local_api_socket_path(),
        "expose_port",
        serde_json::json!({"stream": parked_stream_id, "node": "testpattern", "port": "video", "level": "private"}),
    )
    .json();
    first_runtime.interrupt_and_expect_a_clean_exit();

    assert_eq!(
        exposed,
        serde_json::json!({"stream": parked_stream_id, "node": "testpattern", "port": "video", "level": "private", "recorded": true})
    );
    assert_eq!(the_mode_of(&record_path), 0o600);
    let recorded = machine_root
        .current_stream_record_of(&parked_stream_id)
        .expect("the record stays");
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
        serde_json::json!({"stream": parked_stream_id}),
    )
    .json();
    second_runtime.interrupt_and_expect_a_clean_exit();

    assert_eq!(
        listed["streams"][0]["stream"],
        parked_stream_id.as_str(),
        "{listed}"
    );
    assert_eq!(listed["streams"][0]["state"], "stopped", "{listed}");
    assert_eq!(
        removed,
        serde_json::json!({"stream": parked_stream_id, "unloaded": false, "forgotten": true})
    );
    assert!(
        !record_path.exists(),
        "{} was left behind",
        record_path.display()
    );
}
