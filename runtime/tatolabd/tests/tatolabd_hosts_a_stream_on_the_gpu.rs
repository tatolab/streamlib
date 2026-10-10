// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `tatolabd` hosting a started native-only stream on the GPU: a kept stream
//! re-loaded and started at the runtime's start, the engine's signal ladder
//! over it, and a kept stream that keeps crashing the runtime failed while
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
    TemporaryRuntimeUnit, a_kept_stream_record, a_native_only_stream_graph,
    a_stream_graph_crashing_while, an_executable_standing_in_for_the_interpreter,
    call_a_tool_over_the_local_api, expect_the_runtime_to_crash_on_segv,
    the_local_apis_answer_to_get,
};

/// One engine on the GPU at a time, so the tests' runs never contend for it.
static ONE_STREAM_ON_THE_GPU_AT_A_TIME: Mutex<()> = Mutex::new(());

const THE_KEPT_STREAM_STARTED_LOG_LINE: &str = "[start] The stream `main` started";
const THE_KEPT_STREAM_STARTS_WITHIN: Duration = Duration::from_secs(60);

/// A `tatolabd` on a fresh machine whose kept stream `main` is re-loaded and
/// started at its start.
fn start_tatolabd_re_loading_the_kept_native_only_stream(
    runtime_unit: &TemporaryRuntimeUnit,
    machine_root: &TatolabTestMachineRoot,
) -> SpawnedTatolabd {
    machine_root
        .kept_stream_records()
        .write(&a_kept_stream_record(
            "main",
            a_native_only_stream_graph("main"),
            &machine_root.project_directory(),
            &an_executable_standing_in_for_the_interpreter(),
            false,
        ))
        .unwrap();
    let mut runtime = SpawnedTatolabd::spawn(machine_root.tatolabd_command(&runtime_unit.tatolabd));
    runtime.wait_until_standard_error_carries(
        THE_KEPT_STREAM_STARTED_LOG_LINE,
        THE_KEPT_STREAM_STARTS_WITHIN,
    );
    runtime
}

#[test]
fn a_kept_stream_is_re_loaded_and_started_at_the_start_and_stopped_cleanly_by_an_interrupt() {
    let _one_at_a_time = ONE_STREAM_ON_THE_GPU_AT_A_TIME
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let runtime_unit = TemporaryRuntimeUnit::with_its_lend();
    let machine_root = TatolabTestMachineRoot::new();
    let mut runtime =
        start_tatolabd_re_loading_the_kept_native_only_stream(&runtime_unit, &machine_root);

    let graph = the_local_apis_answer_to_get(&machine_root.local_api_socket_path(), "/api/graph");
    let listed = call_a_tool_over_the_local_api(
        &machine_root.local_api_socket_path(),
        "list_streams",
        serde_json::json!({}),
    )
    .json();
    runtime.interrupt_and_expect_a_clean_exit();

    assert_eq!(graph["streams"][0]["stream"], "main", "{graph}");
    assert_eq!(
        listed["streams"],
        serde_json::json!([{
            "name": "main",
            "state": "kept",
            "project_directory": machine_root.project_directory().display().to_string(),
            "node_count": 1,
            "failed_because": null,
        }])
    );
    assert!(
        runtime
            .standard_error()
            .contains("[stop] The stream `main` stopped"),
        "{}",
        runtime.standard_error()
    );
    assert!(
        !machine_root
            .kept_stream_records()
            .read("main")
            .unwrap()
            .unwrap()
            .stopped,
        "a runtime's stop leaves its kept streams to re-load at the next start"
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
    let mut runtime =
        start_tatolabd_re_loading_the_kept_native_only_stream(&runtime_unit, &machine_root);

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

/// A kept stream crashing the runtime as it loads, twice in a row, is failed
/// at the next start while the other kept stream comes back running; `start`
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
    let kept_stream_records = machine_root.kept_stream_records();
    for kept_stream_record in [
        a_kept_stream_record(
            "crasher",
            a_stream_graph_crashing_while("crasher", &crash_trigger),
            &machine_root.project_directory(),
            &an_executable_standing_in_for_the_interpreter(),
            false,
        ),
        a_kept_stream_record(
            "main",
            a_native_only_stream_graph("main"),
            &machine_root.project_directory(),
            &an_executable_standing_in_for_the_interpreter(),
            false,
        ),
    ] {
        kept_stream_records.write(&kept_stream_record).unwrap();
    }

    for _ in 0..2 {
        expect_the_runtime_to_crash_on_segv(machine_root.tatolabd_command(&runtime_unit.tatolabd));
    }
    let mut runtime = SpawnedTatolabd::spawn(machine_root.tatolabd_command(&runtime_unit.tatolabd));
    runtime.wait_until_standard_error_carries(
        THE_KEPT_STREAM_STARTED_LOG_LINE,
        THE_KEPT_STREAM_STARTS_WITHIN,
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
        serde_json::json!({"stream": "crasher"}),
    )
    .json();
    let listed_after_the_retry = call_a_tool_over_the_local_api(
        &machine_root.local_api_socket_path(),
        "list_streams",
        serde_json::json!({}),
    )
    .json();
    runtime.interrupt_and_expect_a_clean_exit();

    let crasher = &listed_after_two_crashes["streams"][0];
    assert_eq!(crasher["name"], "crasher", "{listed_after_two_crashes}");
    assert_eq!(crasher["state"], "failed", "{listed_after_two_crashes}");
    assert!(
        crasher["failed_because"]
            .as_str()
            .is_some_and(|failed_because| failed_because.contains("last 2 crashes in a row")),
        "{listed_after_two_crashes}"
    );
    let main = &listed_after_two_crashes["streams"][1];
    assert_eq!(
        (&main["name"], &main["state"], &main["node_count"]),
        (&"main".into(), &"kept".into(), &1.into()),
        "{listed_after_two_crashes}"
    );
    assert_eq!(started["stream"], "crasher", "{started}");
    let crasher = &listed_after_the_retry["streams"][0];
    assert_eq!(
        (
            &crasher["state"],
            &crasher["node_count"],
            &crasher["failed_because"]
        ),
        (&"kept".into(), &1.into(), &serde_json::Value::Null),
        "{listed_after_the_retry}"
    );
    let retried = kept_stream_records.read("crasher").unwrap().unwrap();
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
    let kept_stream_records = machine_root.kept_stream_records();
    kept_stream_records
        .write(&a_kept_stream_record(
            "crasher",
            a_stream_graph_crashing_while("crasher", &crash_trigger),
            &machine_root.project_directory(),
            &an_executable_standing_in_for_the_interpreter(),
            false,
        ))
        .unwrap();

    expect_the_runtime_to_crash_on_segv(machine_root.tatolabd_command(&runtime_unit.tatolabd));
    std::fs::remove_file(&crash_trigger).unwrap();
    let mut runtime = SpawnedTatolabd::spawn(machine_root.tatolabd_command(&runtime_unit.tatolabd));
    runtime.wait_until_standard_error_carries(
        "[start] The stream `crasher` started",
        THE_KEPT_STREAM_STARTS_WITHIN,
    );
    let count_after_one_crash = kept_stream_records
        .read("crasher")
        .unwrap()
        .unwrap()
        .runtime_crashes_in_a_row_implicating_it;
    runtime.interrupt_and_expect_a_clean_exit();
    std::fs::write(&crash_trigger, "").unwrap();
    for _ in 0..2 {
        expect_the_runtime_to_crash_on_segv(machine_root.tatolabd_command(&runtime_unit.tatolabd));
    }

    assert_eq!(count_after_one_crash, 1);
    let after_the_clean_restart = kept_stream_records.read("crasher").unwrap().unwrap();
    assert_eq!(
        after_the_clean_restart.runtime_crashes_in_a_row_implicating_it, 1,
        "the clean stop reset the count, so only the crash after it was counted"
    );
    assert!(!after_the_clean_restart.is_failed());
}
