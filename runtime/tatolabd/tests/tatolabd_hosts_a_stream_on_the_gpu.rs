// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `tatolabd` hosting a started native-only stream on the GPU: a recorded stream
//! re-loaded and started at the runtime's start, the engine's signal ladder
//! over it, and a recorded stream that keeps crashing the runtime failed while
//! every other comes back. Rig-only:
//! `cargo test -p tatolabd --features hardware-tests,machine-directories-under-a-test-root`.

#![cfg(all(
    feature = "hardware-tests",
    feature = "machine-directories-under-a-test-root"
))]

mod common;

use std::sync::Mutex;
use std::time::Duration;

use common::{
    AN_INTERRUPTED_RUNTIME_EXITS_WITHIN, SpawnedTatolabd, TatolabTestMachineRoot,
    TemporaryRuntimeUnit, a_native_only_stream_graph, a_stream_graph_crashing_while,
    a_stream_record, an_executable_standing_in_for_the_interpreter, call_a_tool_over_the_local_api,
    expect_the_runtime_to_crash_on_segv, the_local_apis_answer_to_get,
};

/// One engine on the GPU at a time, so the tests' runs never contend for it.
static ONE_STREAM_ON_THE_GPU_AT_A_TIME: Mutex<()> = Mutex::new(());

const THE_RECORDED_STREAM_STARTS_WITHIN: Duration = Duration::from_secs(60);

/// The line the runtime logs once the stream `stream_id` started.
fn the_stream_started_log_line(stream_id: &str) -> String {
    format!("[start] The stream `{stream_id}` started")
}

/// A `tatolabd` on a fresh machine whose recorded stream, the function `main`, is
/// re-loaded and started at its start; returns it and the stream's id.
fn start_tatolabd_re_loading_the_recorded_native_only_stream(
    runtime_unit: &TemporaryRuntimeUnit,
    machine_root: &TatolabTestMachineRoot,
) -> (SpawnedTatolabd, String) {
    let main = a_stream_record(
        "main",
        a_native_only_stream_graph("main"),
        &machine_root.project_directory(),
        &an_executable_standing_in_for_the_interpreter(),
        false,
    );
    machine_root.stream_records().write(&main).unwrap();
    let mut runtime = SpawnedTatolabd::spawn(machine_root.tatolabd_command(&runtime_unit.tatolabd));
    runtime.wait_until_standard_error_carries(
        &the_stream_started_log_line(&main.stream_id),
        THE_RECORDED_STREAM_STARTS_WITHIN,
    );
    (runtime, main.stream_id)
}

#[test]
fn a_recorded_stream_is_re_loaded_and_started_at_the_start_and_stopped_cleanly_by_an_interrupt() {
    let _one_at_a_time = ONE_STREAM_ON_THE_GPU_AT_A_TIME
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let runtime_unit = TemporaryRuntimeUnit::with_its_lend();
    let machine_root = TatolabTestMachineRoot::new();
    let (mut runtime, main_stream_id) =
        start_tatolabd_re_loading_the_recorded_native_only_stream(&runtime_unit, &machine_root);

    let graph = the_local_apis_answer_to_get(&machine_root.local_api_socket_path(), "/api/graph");
    let listed = call_a_tool_over_the_local_api(
        &machine_root.local_api_socket_path(),
        "list_streams",
        serde_json::json!({}),
    )
    .json();
    runtime.interrupt_and_expect_a_clean_exit();

    assert_eq!(
        graph["streams"][0]["stream"],
        main_stream_id.as_str(),
        "{graph}"
    );
    assert_eq!(
        listed["streams"],
        serde_json::json!([{
            "stream": main_stream_id,
            "display_name": "main",
            "location": {
                "project_directory": machine_root.project_directory(),
                "stream_function": "stream:main",
            },
            "state": "running",
            "node_count": 1,
            "failed_because": null,
        }])
    );
    assert!(
        runtime
            .standard_error()
            .contains(&format!("[stop] The stream `{main_stream_id}` stopped")),
        "{}",
        runtime.standard_error()
    );
    assert!(
        !machine_root
            .current_stream_record_of(&main_stream_id)
            .expect("the record stays")
            .stopped,
        "a runtime's stop leaves its recorded streams to re-load at the next start"
    );
}

/// The engine's ladder, reused as-is: graceful, forced, then the process gone
/// with status 130. The interrupts land within the run loop's own poll
/// interval, so the third arrives while the engine still owns the signals.
#[test]
fn a_third_interrupt_ends_the_process_with_status_130() {
    let _one_at_a_time = ONE_STREAM_ON_THE_GPU_AT_A_TIME
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let runtime_unit = TemporaryRuntimeUnit::with_its_lend();
    let machine_root = TatolabTestMachineRoot::new();
    let (mut runtime, _main_stream_id) =
        start_tatolabd_re_loading_the_recorded_native_only_stream(&runtime_unit, &machine_root);

    for _ in 0..3 {
        runtime.deliver(libc::SIGINT);
        // Apart, so the kernel does not merge two pending interrupts into one.
        std::thread::sleep(Duration::from_millis(5));
    }
    let status = runtime.wait_for_exit_within(AN_INTERRUPTED_RUNTIME_EXITS_WITHIN);

    assert_eq!(
        status.code(),
        Some(130),
        "a third interrupt exits 130, got {status}:\n{}",
        runtime.standard_error()
    );
    assert!(
        !machine_root.runtime_run_in_progress_record_path().exists(),
        "the owner's third interrupt is a stop, and left a crash for the next start to count"
    );
}

/// A recorded stream crashing the runtime as it loads, twice in a row, is failed
/// at the next start while the other recorded stream comes back running; `start`
/// brings it back once it no longer crashes.
#[test]
fn a_stream_crashing_the_runtime_twice_is_failed_every_other_comes_back_and_start_retries_it() {
    let _one_at_a_time = ONE_STREAM_ON_THE_GPU_AT_A_TIME
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let runtime_unit = TemporaryRuntimeUnit::with_its_lend();
    let machine_root = TatolabTestMachineRoot::new();
    let crash_trigger = machine_root.project_directory().join("crash-armed");
    std::fs::write(&crash_trigger, "").unwrap();
    let crasher = a_stream_record(
        "crasher",
        a_stream_graph_crashing_while("crasher", &crash_trigger),
        &machine_root.project_directory(),
        &an_executable_standing_in_for_the_interpreter(),
        false,
    );
    let main = a_stream_record(
        "main",
        a_native_only_stream_graph("main"),
        &machine_root.project_directory(),
        &an_executable_standing_in_for_the_interpreter(),
        false,
    );
    for record in [&crasher, &main] {
        machine_root.stream_records().write(record).unwrap();
    }

    for _ in 0..2 {
        expect_the_runtime_to_crash_on_segv(machine_root.tatolabd_command(&runtime_unit.tatolabd));
    }
    let mut runtime = SpawnedTatolabd::spawn(machine_root.tatolabd_command(&runtime_unit.tatolabd));
    runtime.wait_until_standard_error_carries(
        &the_stream_started_log_line(&main.stream_id),
        THE_RECORDED_STREAM_STARTS_WITHIN,
    );
    let listed_after_two_crashes = call_a_tool_over_the_local_api(
        &machine_root.local_api_socket_path(),
        "list_streams",
        serde_json::json!({}),
    )
    .json();
    std::fs::remove_file(&crash_trigger).unwrap();
    let started = call_a_tool_over_the_local_api(
        &machine_root.local_api_socket_path(),
        "start_stream",
        serde_json::json!({"stream": crasher.stream_id}),
    )
    .json();
    let listed_after_the_retry = call_a_tool_over_the_local_api(
        &machine_root.local_api_socket_path(),
        "list_streams",
        serde_json::json!({}),
    )
    .json();
    runtime.interrupt_and_expect_a_clean_exit();

    let listed_crasher = &listed_after_two_crashes["streams"][0];
    assert_eq!(
        listed_crasher["stream"],
        crasher.stream_id.as_str(),
        "{listed_after_two_crashes}"
    );
    assert_eq!(
        listed_crasher["state"], "failed",
        "{listed_after_two_crashes}"
    );
    assert!(
        listed_crasher["failed_because"]
            .as_str()
            .is_some_and(|failed_because| failed_because.contains("last 2 crashes in a row")),
        "{listed_after_two_crashes}"
    );
    let listed_main = &listed_after_two_crashes["streams"][1];
    assert_eq!(
        (
            &listed_main["stream"],
            &listed_main["state"],
            &listed_main["node_count"]
        ),
        (
            &main.stream_id.as_str().into(),
            &"running".into(),
            &1.into()
        ),
        "{listed_after_two_crashes}"
    );
    assert_eq!(started["stream"], crasher.stream_id.as_str(), "{started}");
    let retried_crasher = &listed_after_the_retry["streams"][0];
    assert_eq!(
        (
            &retried_crasher["state"],
            &retried_crasher["node_count"],
            &retried_crasher["failed_because"]
        ),
        (&"running".into(), &1.into(), &serde_json::Value::Null),
        "{listed_after_the_retry}"
    );
    let retried = machine_root
        .current_stream_record_of(&crasher.stream_id)
        .expect("the crasher's record stays");
    assert_eq!(retried.runtime_crashes_in_a_row_implicating_it, 0);
    assert!(!retried.is_failed());
}

/// A clean stop of the runtime between two crashes resets the stream's count,
/// so it is not failed.
#[test]
fn a_clean_restart_between_two_crashes_resets_the_count() {
    let _one_at_a_time = ONE_STREAM_ON_THE_GPU_AT_A_TIME
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let runtime_unit = TemporaryRuntimeUnit::with_its_lend();
    let machine_root = TatolabTestMachineRoot::new();
    let crash_trigger = machine_root.project_directory().join("crash-armed");
    std::fs::write(&crash_trigger, "").unwrap();
    let crasher = a_stream_record(
        "crasher",
        a_stream_graph_crashing_while("crasher", &crash_trigger),
        &machine_root.project_directory(),
        &an_executable_standing_in_for_the_interpreter(),
        false,
    );
    machine_root.stream_records().write(&crasher).unwrap();

    expect_the_runtime_to_crash_on_segv(machine_root.tatolabd_command(&runtime_unit.tatolabd));
    std::fs::remove_file(&crash_trigger).unwrap();
    let mut runtime = SpawnedTatolabd::spawn(machine_root.tatolabd_command(&runtime_unit.tatolabd));
    runtime.wait_until_standard_error_carries(
        &the_stream_started_log_line(&crasher.stream_id),
        THE_RECORDED_STREAM_STARTS_WITHIN,
    );
    let count_after_one_crash = machine_root
        .current_stream_record_of(&crasher.stream_id)
        .expect("the crasher's record stays")
        .runtime_crashes_in_a_row_implicating_it;
    runtime.interrupt_and_expect_a_clean_exit();
    std::fs::write(&crash_trigger, "").unwrap();
    for _ in 0..2 {
        expect_the_runtime_to_crash_on_segv(machine_root.tatolabd_command(&runtime_unit.tatolabd));
    }

    assert_eq!(count_after_one_crash, 1);
    let after_the_clean_restart = machine_root
        .current_stream_record_of(&crasher.stream_id)
        .expect("the crasher's record stays");
    assert_eq!(
        after_the_clean_restart.runtime_crashes_in_a_row_implicating_it, 1,
        "the clean stop reset the count, so only the crash after it was counted"
    );
    assert!(!after_the_clean_restart.is_failed());
}
