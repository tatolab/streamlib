// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! A kept stream that keeps crashing `tatolabd` is recorded failed, GPU-free:
//! the crash-on-demand test node crashes the runtime while its stream loads,
//! twice, and the next start lists the stream failed with its reason and
//! skips it; `start_stream` retries it. Built with
//! `--features machine-directories-under-a-test-root`, which registers the
//! node.

#![cfg(feature = "machine-directories-under-a-test-root")]

mod common;

use common::{
    A_RUNTIME_STARTS_SERVING_WITHIN, SpawnedTatolabd, TatolabTestMachineRoot, TemporaryRuntimeUnit,
    a_kept_stream_record, an_executable_standing_in_for_the_interpreter,
    call_a_tool_over_the_local_api, run_to_exit_within,
};

#[test]
fn a_kept_stream_crashing_the_runtime_twice_is_failed_named_with_its_reason_and_start_retries_it() {
    let runtime_unit = TemporaryRuntimeUnit::with_its_lend();
    let machine_root = TatolabTestMachineRoot::new();
    let kept_stream_records = machine_root.kept_stream_records();
    let crash_trigger = machine_root.project_directory().join("crash-armed");
    std::fs::write(&crash_trigger, "").unwrap();
    kept_stream_records
        .write(&a_kept_stream_record(
            "crasher",
            a_stream_graph_crashing_while("crasher", &crash_trigger),
            &machine_root.project_directory(),
            &an_executable_standing_in_for_the_interpreter(),
            false,
        ))
        .unwrap();
    let run_in_progress_record = machine_root
        .state_directory()
        .join("runtime-run-in-progress");

    expect_the_runtime_to_crash_on_segv(
        machine_root.tatolabd_command_with_no_vulkan_driver(&runtime_unit.tatolabd),
    );
    assert_eq!(
        std::fs::read_to_string(&run_in_progress_record).unwrap(),
        "crasher\tSIGSEGV\n",
        "the crash was not pinned on the stream that was loading"
    );
    expect_the_runtime_to_crash_on_segv(
        machine_root.tatolabd_command_with_no_vulkan_driver(&runtime_unit.tatolabd),
    );
    let after_two_crashes = kept_stream_records.read("crasher").unwrap().unwrap();
    assert_eq!(after_two_crashes.runtime_crashes_in_a_row_implicating_it, 1);
    assert!(!after_two_crashes.is_failed());

    let mut runtime = SpawnedTatolabd::spawn_and_wait_until_serving(
        machine_root.tatolabd_command_with_no_vulkan_driver(&runtime_unit.tatolabd),
    );
    let listed = call_a_tool_over_the_local_api(
        &machine_root.local_api_socket_path(),
        "list_streams",
        serde_json::json!({}),
    )
    .json();
    assert_eq!(listed["streams"][0]["name"], "crasher", "{listed}");
    assert_eq!(listed["streams"][0]["state"], "failed", "{listed}");
    let failed_because = listed["streams"][0]["failed_because"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    assert!(
        failed_because.contains("the runtime's last 2 crashes in a row")
            && failed_because.contains("SIGSEGV"),
        "{listed}"
    );
    let standard_error = runtime.standard_error();
    assert!(
        standard_error.contains("the kept stream `crasher` is failed, so it is not re-loaded"),
        "{standard_error}"
    );

    std::fs::remove_file(&crash_trigger).unwrap();
    let retried = call_a_tool_over_the_local_api(
        &machine_root.local_api_socket_path(),
        "start_stream",
        serde_json::json!({"stream": "crasher"}),
    );
    runtime.interrupt_and_expect_a_clean_exit();

    assert!(
        retried.is_error,
        "a stream's start needs the GPU this runtime was left without: {}",
        retried.text
    );
    assert!(
        !run_in_progress_record.exists(),
        "a clean stop left the run-in-progress record behind"
    );
    let after_the_retry = kept_stream_records.read("crasher").unwrap().unwrap();
    assert_eq!(after_the_retry.runtime_crashes_in_a_row_implicating_it, 0);
    let failed_because = after_the_retry
        .failed_because
        .expect("a refused retry stays failed");
    assert!(
        failed_because.contains("`start` retried it, and it did not load"),
        "the retry did not load the stream past its crash: {failed_because}"
    );
    assert!(!failed_because.contains("SIGSEGV"), "{failed_because}");
}

#[test]
fn a_kept_stream_whose_venv_was_deleted_comes_back_failed_naming_the_venv() {
    let runtime_unit = TemporaryRuntimeUnit::with_its_lend();
    let machine_root = TatolabTestMachineRoot::new();
    let deleted_interpreter = machine_root.project_directory().join(".venv/bin/python");
    machine_root
        .kept_stream_records()
        .write(&a_kept_stream_record(
            "orphan",
            common::a_native_only_stream_graph("orphan"),
            &machine_root.project_directory(),
            &deleted_interpreter,
            false,
        ))
        .unwrap();

    let mut runtime = SpawnedTatolabd::spawn_and_wait_until_serving(
        machine_root.tatolabd_command_with_no_vulkan_driver(&runtime_unit.tatolabd),
    );
    let listed = call_a_tool_over_the_local_api(
        &machine_root.local_api_socket_path(),
        "list_streams",
        serde_json::json!({}),
    )
    .json();
    runtime.interrupt_and_expect_a_clean_exit();

    assert_eq!(listed["streams"][0]["state"], "failed", "{listed}");
    let failed_because = listed["streams"][0]["failed_because"]
        .as_str()
        .unwrap_or_default();
    assert!(
        failed_because.contains(&format!(
            "its interpreter {} is gone",
            deleted_interpreter.display()
        )),
        "{listed}"
    );
}
