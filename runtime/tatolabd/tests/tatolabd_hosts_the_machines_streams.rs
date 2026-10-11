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

/// A `tailscale` standing in for the Tailscale CLI, alone in a directory a
/// test puts on `PATH`, writing `status_json` for any arguments with the
/// shell's own `printf`, since nothing else is on that `PATH`.
fn a_directory_holding_a_tailscale_cli_writing(
    machine_root: &TatolabTestMachineRoot,
    status_json: &serde_json::Value,
) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let status_json = status_json.to_string();
    assert!(
        !status_json.contains('\''),
        "the stub quotes the status in single quotes"
    );
    let directory = machine_root.path().join("path-with-a-stub-tailscale");
    std::fs::create_dir_all(&directory).unwrap();
    let tailscale_cli = directory.join("tailscale");
    std::fs::write(
        &tailscale_cli,
        format!("#!/bin/sh\nprintf '%s' '{status_json}'\n"),
    )
    .unwrap();
    std::fs::set_permissions(&tailscale_cli, std::fs::Permissions::from_mode(0o700)).unwrap();
    directory
}

/// This machine's hostname: the kernel's UTS node name, which `gethostname`
/// returns on Linux.
#[cfg(target_os = "linux")]
fn this_hosts_name() -> String {
    std::fs::read_to_string("/proc/sys/kernel/hostname")
        .expect("the test host reports a hostname")
        .trim_end_matches('\n')
        .to_string()
}

/// What `/api/graph` answers a `tatolabd` started with `path` as its `PATH`.
fn the_machine_wide_graph_with_path(
    machine_root: &TatolabTestMachineRoot,
    runtime_unit: &TemporaryRuntimeUnit,
    path: &std::path::Path,
) -> serde_json::Value {
    let mut tatolabd_command = machine_root.tatolabd_command(&runtime_unit.tatolabd);
    tatolabd_command.env("PATH", path);
    let mut runtime = SpawnedTatolabd::spawn_and_wait_until_serving(tatolabd_command);
    let graph = the_local_apis_answer_to_get(&machine_root.local_api_socket_path(), "/api/graph");
    runtime.interrupt_and_expect_a_clean_exit();
    graph
}

/// On a tailnet the machine is named by its tailnet name, and `graph` with no
/// stream renders it over every loaded stream — none here.
#[test]
fn on_a_tailnet_graph_answers_the_machine_by_its_tailnet_name_and_no_streams() {
    let runtime_unit = TemporaryRuntimeUnit::with_its_lend();
    let machine_root = TatolabTestMachineRoot::new();
    let path = a_directory_holding_a_tailscale_cli_writing(
        &machine_root,
        &serde_json::json!({
            "BackendState": "Running",
            "Self": { "HostName": "Desk", "DNSName": "desk.tail2950db.ts.net." },
        }),
    );

    assert_eq!(
        the_machine_wide_graph_with_path(&machine_root, &runtime_unit, &path),
        serde_json::json!({ "machine": "desk", "streams": [] })
    );
}

/// With Tailscale stopped, or with no Tailscale at all, the machine is named
/// by its hostname, cast.
///
/// Linux only: on macOS the runtime also reads the Tailscale app's own CLI,
/// which is found whatever `PATH` says.
#[cfg(target_os = "linux")]
#[test]
fn with_tailscale_stopped_or_absent_graph_answers_the_machine_by_its_hostname_cast() {
    let runtime_unit = TemporaryRuntimeUnit::with_its_lend();
    let machine_root = TatolabTestMachineRoot::new();
    let the_hostname_cast =
        streamlib::sdk::runtime::MachineName::from_a_hostname(&this_hosts_name())
            .expect("the test host's hostname names something")
            .name()
            .to_string();
    let stopped = a_directory_holding_a_tailscale_cli_writing(
        &machine_root,
        &serde_json::json!({
            "BackendState": "Stopped",
            "Self": { "DNSName": "desk.tail2950db.ts.net." },
        }),
    );
    let holding_no_tailscale = machine_root.path().join("path-with-no-tailscale");
    std::fs::create_dir_all(&holding_no_tailscale).unwrap();

    for path in [stopped, holding_no_tailscale] {
        assert_eq!(
            the_machine_wide_graph_with_path(&machine_root, &runtime_unit, &path),
            serde_json::json!({ "machine": the_hostname_cast, "streams": [] }),
            "PATH {path:?}"
        );
    }
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
/// failed, reporting each by name: a stopped one is left unloaded, one whose
/// interpreter is gone is recorded failed for it, and one the engine could
/// not start — no GPU — stays kept for the next start, the runtime still up.
#[test]
fn every_kept_stream_not_stopped_is_re_loaded_at_the_start_and_one_that_cannot_is_named() {
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
        standard_error.contains(
            "the kept stream `front` was not re-loaded, and stays kept for the runtime's next start"
        ),
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
    let orphan_failed_because = listed_streams[1]["failed_because"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    assert!(
        orphan_failed_because.contains(&format!(
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
            serde_json::json!({"name": "front", "state": "kept", "project_directory": project_directory, "node_count": null, "failed_because": null}),
            serde_json::json!({"name": "orphan", "state": "failed", "project_directory": project_directory, "node_count": null, "failed_because": orphan_failed_because}),
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
