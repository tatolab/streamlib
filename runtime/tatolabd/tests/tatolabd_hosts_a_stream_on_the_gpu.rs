// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `tatolabd` hosting a started native-only stream on the GPU: a kept stream
//! re-loaded and started at the runtime's start, and the engine's signal
//! ladder over it. Rig-only:
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
    an_executable_standing_in_for_the_interpreter, call_a_tool_over_the_local_api,
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
}
