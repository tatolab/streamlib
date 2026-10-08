// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `tatolabd` hosting a native-only stream on the GPU: the engine's signal
//! ladder, and the runtime name reaching the local API. Rig-only:
//! `cargo test -p tatolabd --features hardware-tests`.

#![cfg(feature = "hardware-tests")]

mod common;

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use common::{
    SpawnedTatolabd, TatolabdRunState, TemporaryRuntimeUnit,
    an_executable_standing_in_for_the_interpreter,
};

/// One engine on the GPU at a time, so the tests' runs never contend for it.
static ONE_STREAM_ON_THE_GPU_AT_A_TIME: Mutex<()> = Mutex::new(());

const THE_ENGINE_STARTED_LOG_LINE: &str = "[start] Runtime started";
const THE_ENGINE_STARTS_WITHIN: Duration = Duration::from_secs(60);
const A_STREAM_STOPS_WITHIN: Duration = Duration::from_secs(30);

/// A stream of one test pattern and nothing that needs a display.
fn a_native_only_stream_graph() -> serde_json::Value {
    serde_json::json!({
        "stream": "main",
        "nodes": [{
            "name": "testpattern",
            "type": "tatolab.stream:TestPatternSource",
            "config": {"width": 320, "height": 240},
        }],
    })
}

fn start_tatolabd_hosting_the_native_only_stream(
    tatolabd: &Path,
    run_state: &TatolabdRunState,
    extra_environment: &[(&str, &str)],
) -> SpawnedTatolabd {
    let stream_graph_file = run_state.write_stream_graph(&a_native_only_stream_graph());
    let mut tatolabd_command = run_state.tatolabd_command(tatolabd);
    tatolabd_command
        .arg("--stream-graph")
        .arg(stream_graph_file)
        .arg("--project")
        .arg(run_state.project_directory())
        .arg("--interpreter")
        .arg(an_executable_standing_in_for_the_interpreter());
    for (name, value) in extra_environment {
        tatolabd_command.env(name, value);
    }
    let mut spawned = SpawnedTatolabd::spawn(tatolabd_command);
    spawned
        .wait_until_standard_error_carries(THE_ENGINE_STARTED_LOG_LINE, THE_ENGINE_STARTS_WITHIN);
    spawned
}

#[test]
fn an_interrupt_stops_the_stream_cleanly_and_exits_zero() {
    let _one_at_a_time = ONE_STREAM_ON_THE_GPU_AT_A_TIME
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let runtime_unit = TemporaryRuntimeUnit::with_its_lend();
    let run_state = TatolabdRunState::new();
    let mut spawned =
        start_tatolabd_hosting_the_native_only_stream(&runtime_unit.tatolabd, &run_state, &[]);

    spawned.deliver(libc::SIGINT);
    let status = spawned.wait_for_exit_within(A_STREAM_STOPS_WITHIN);

    assert_eq!(
        status.code(),
        Some(0),
        "a graceful interrupt exits 0, got {status}:\n{}",
        spawned.standard_error()
    );
    assert!(
        spawned
            .standard_error()
            .contains("[stop] Graceful shutdown complete"),
        "{}",
        spawned.standard_error()
    );
    assert_eq!(
        spawned.standard_output(),
        "",
        "standard output carries nothing"
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
    let run_state = TatolabdRunState::new();
    let mut spawned =
        start_tatolabd_hosting_the_native_only_stream(&runtime_unit.tatolabd, &run_state, &[]);

    for _ in 0..3 {
        spawned.deliver(libc::SIGINT);
        // Apart, so the kernel does not merge two pending interrupts into one.
        std::thread::sleep(Duration::from_millis(5));
    }
    let status = spawned.wait_for_exit_within(A_STREAM_STOPS_WITHIN);

    assert_eq!(
        status.code(),
        Some(130),
        "a third interrupt exits 130, got {status}:\n{}",
        spawned.standard_error()
    );
}

/// `STREAMLIB_RUNTIME_NAME` names the runtime, and the local API's `graph`
/// renders it.
#[test]
fn the_runtime_name_from_the_environment_reaches_the_local_apis_graph() {
    let _one_at_a_time = ONE_STREAM_ON_THE_GPU_AT_A_TIME
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let runtime_unit = TemporaryRuntimeUnit::with_its_lend();
    let run_state = TatolabdRunState::new();
    let pinned_runtime_id = "Rtatolabdruntimenametest";
    let mut spawned = start_tatolabd_hosting_the_native_only_stream(
        &runtime_unit.tatolabd,
        &run_state,
        &[
            (
                "STREAMLIB_RUNTIME_NAME",
                "tatolabd-named-from-the-environment",
            ),
            ("STREAMLIB_RUNTIME_ID", pinned_runtime_id),
        ],
    );

    let graph = the_local_apis_graph(
        &run_state
            .runtime_directory()
            .join(format!("local-api-{pinned_runtime_id}.sock")),
    );
    spawned.deliver(libc::SIGINT);
    let status = spawned.wait_for_exit_within(A_STREAM_STOPS_WITHIN);

    assert_eq!(
        graph["runtime_name"], "tatolabd-named-from-the-environment",
        "{graph}"
    );
    assert_eq!(graph["stream"], "main", "{graph}");
    assert_eq!(status.code(), Some(0), "{}", spawned.standard_error());
}

/// `GET /api/graph` over the local API socket, retried until the socket
/// answers.
fn the_local_apis_graph(local_api_socket_path: &Path) -> serde_json::Value {
    let deadline = Instant::now() + THE_ENGINE_STARTS_WITHIN;
    loop {
        if let Some(graph) = get_the_graph_once(local_api_socket_path) {
            return graph;
        }
        assert!(
            Instant::now() < deadline,
            "the local API at {} did not answer `GET /api/graph`",
            local_api_socket_path.display()
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn get_the_graph_once(local_api_socket_path: &Path) -> Option<serde_json::Value> {
    let mut local_api = UnixStream::connect(local_api_socket_path).ok()?;
    local_api
        .write_all(b"GET /api/graph HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .ok()?;
    let mut response = String::new();
    local_api.read_to_string(&mut response).ok()?;
    let (head, body) = response.split_once("\r\n\r\n")?;
    if !head.starts_with("HTTP/1.1 200") {
        return None;
    }
    serde_json::from_str(body).ok()
}
