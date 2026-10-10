// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `tatolab run` attached and `tatolab dev` run as a user runs them, against a stub local API at
//! an isolated machine's fixed socket: the load over a `/mcp/stdio` connection of the verb's own,
//! the records followed by sequence number and printed as the runtime mirrors them, Ctrl-C
//! stopping the stream, the runtime closing the connection, a refused load, and `dev` loading
//! again on a saved edit and after the runtime drops it.

#![cfg(any(target_os = "linux", feature = "machine-directories-under-a-test-root"))]

mod common;

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde_json::json;

use common::isolated_machine_directories::IsolatedMachineDirectories;
use common::running_tatolab::{RunningTatolab, wait_until};
use common::runtime_log_line_fixtures::a_log_record;
use common::stub_local_api_server::{
    StubLocalApiScript, StubLocalApiServer, StubStreamLogRecords, StubToolAnswer,
    run_stream_tool_result_text, stop_stream_tool_result_text,
};

/// The stream every attached load here is loaded as.
const ATTACHED_STREAM_NAME: &str = "cam";

/// A project directory of a test's own, holding a `stream.py`, and its canonical path.
struct ScratchProject {
    project_directory: tempfile::TempDir,
}

impl ScratchProject {
    fn new() -> Self {
        let project_directory = tempfile::tempdir().unwrap();
        std::fs::write(
            project_directory.path().join("stream.py"),
            "# the first version\n",
        )
        .unwrap();
        Self { project_directory }
    }

    fn path_as_given(&self) -> &str {
        self.project_directory.path().to_str().unwrap()
    }

    fn canonical_path(&self) -> PathBuf {
        self.project_directory.path().canonicalize().unwrap()
    }

    fn save_stream_py(&self, new_contents: &str) {
        std::fs::write(
            self.project_directory.path().join("stream.py"),
            new_contents,
        )
        .unwrap();
    }
}

fn loaded_answer(project_directory: &Path) -> StubToolAnswer {
    StubToolAnswer::tool_result(&run_stream_tool_result_text(
        ATTACHED_STREAM_NAME,
        false,
        project_directory,
        3,
    ))
}

fn stopped_answer() -> StubToolAnswer {
    StubToolAnswer::tool_result(&stop_stream_tool_result_text(ATTACHED_STREAM_NAME, false))
}

/// A runtime that loads the stream as `run_stream_answers` say, in order, stops it, and pages
/// `stream_log_records` one record per `logs` call.
fn a_runtime_holding(
    isolated_machine_directories: &IsolatedMachineDirectories,
    run_stream_answers: Vec<StubToolAnswer>,
    stream_log_records: Vec<serde_json::Value>,
) -> StubLocalApiServer {
    isolated_machine_directories.serve_stub_local_api(StubLocalApiScript {
        tool_answers_by_name: HashMap::from([
            ("run_stream".to_owned(), run_stream_answers),
            ("stop_stream".to_owned(), vec![stopped_answer()]),
        ]),
        stream_log_records: Some(StubStreamLogRecords {
            records: stream_log_records,
            page_size: 1,
        }),
        ..StubLocalApiScript::default()
    })
}

/// The tools called, in order, less the `logs` polls between them.
fn tool_calls_besides_logs(stub_local_api_server: &StubLocalApiServer) -> Vec<String> {
    stub_local_api_server
        .recorded_tool_calls()
        .into_iter()
        .map(|recorded_tool_call| recorded_tool_call.tool_name)
        .filter(|tool_name| tool_name != "logs")
        .collect()
}

fn calls_to(stub_local_api_server: &StubLocalApiServer, tool_name: &str) -> usize {
    stub_local_api_server.recorded_arguments_of(tool_name).len()
}

fn rendered_line(message: &str) -> String {
    format!("21:04:27.573 [ INFO] [Rabc/rust] tatolabd — {message}")
}

#[test]
fn run_loads_the_stream_attached_and_prints_each_record_as_the_runtime_mirrors_it() {
    let isolated_machine_directories = IsolatedMachineDirectories::new();
    let scratch_project = ScratchProject::new();
    let stub_local_api_server = a_runtime_holding(
        &isolated_machine_directories,
        vec![loaded_answer(&scratch_project.canonical_path())],
        vec![
            a_log_record(json!({"message": "first"})),
            a_log_record(json!({"message": "second"})),
        ],
    );

    let mut running_tatolab =
        RunningTatolab::spawn(isolated_machine_directories.tatolab_command(&[
            "run",
            "stream.py:main",
            "--dir",
            scratch_project.path_as_given(),
            "--name",
            ATTACHED_STREAM_NAME,
        ]));

    assert_eq!(
        running_tatolab.next_standard_output_line("the first record"),
        rendered_line("first")
    );
    assert_eq!(
        running_tatolab.next_standard_output_line("the second record"),
        rendered_line("second")
    );
    running_tatolab.wait_for_standard_error_line_containing("cam loaded (3 nodes");
    wait_until("a third `logs` call", || {
        calls_to(&stub_local_api_server, "logs") >= 3
    });
    assert_eq!(
        stub_local_api_server.recorded_arguments_of("run_stream"),
        [json!({
            "project_directory": scratch_project.canonical_path(),
            "stream_function": "stream.py:main",
            "name": ATTACHED_STREAM_NAME,
            "keep": false,
        })]
    );
    assert_eq!(
        stub_local_api_server
            .recorded_mcp_stdio_request_heads()
            .len(),
        1,
        "the load rides a /mcp/stdio connection of the verb's own"
    );
    let logs_afters: Vec<u64> = stub_local_api_server
        .recorded_arguments_of("logs")
        .iter()
        .map(|logs_arguments| {
            assert_eq!(logs_arguments["stream"], ATTACHED_STREAM_NAME);
            logs_arguments["after"].as_u64().unwrap()
        })
        .collect();
    assert_eq!(
        logs_afters[..3],
        [0, 1, 2],
        "each record is asked for after the last one read"
    );
    drop(running_tatolab);
}

#[test]
fn ctrl_c_stops_the_attached_stream_and_exits_zero() {
    let isolated_machine_directories = IsolatedMachineDirectories::new();
    let scratch_project = ScratchProject::new();
    let stub_local_api_server = a_runtime_holding(
        &isolated_machine_directories,
        vec![loaded_answer(&scratch_project.canonical_path())],
        vec![a_log_record(json!({"message": "running"}))],
    );
    let running_tatolab = RunningTatolab::spawn(isolated_machine_directories.tatolab_command(&[
        "run",
        "--dir",
        scratch_project.path_as_given(),
    ]));
    running_tatolab.next_standard_output_line("the stream's record");

    running_tatolab.send_signal(libc::SIGINT);
    let (exit_status, standard_error_lines) = running_tatolab.wait_for_exit();

    assert_eq!(exit_status.code(), Some(0), "{standard_error_lines:?}");
    assert_eq!(
        stub_local_api_server.recorded_arguments_of("stop_stream"),
        [json!({"stream": ATTACHED_STREAM_NAME})]
    );
    assert_eq!(
        tool_calls_besides_logs(&stub_local_api_server),
        ["run_stream", "stop_stream"]
    );
    assert!(
        standard_error_lines
            .iter()
            .any(|standard_error_line| standard_error_line == "tatolab: cam stopped"),
        "{standard_error_lines:?}"
    );
}

#[test]
fn the_runtime_closing_the_connection_ends_run_with_one_naming_its_log() {
    let isolated_machine_directories = IsolatedMachineDirectories::new();
    let scratch_project = ScratchProject::new();
    let stub_local_api_server = a_runtime_holding(
        &isolated_machine_directories,
        vec![loaded_answer(&scratch_project.canonical_path())],
        Vec::new(),
    );
    let running_tatolab = RunningTatolab::spawn(isolated_machine_directories.tatolab_command(&[
        "run",
        "--dir",
        scratch_project.path_as_given(),
    ]));
    wait_until("the stream's records being followed", || {
        calls_to(&stub_local_api_server, "logs") > 0
    });

    stub_local_api_server.close_every_mcp_stdio_connection();
    let (exit_status, standard_error_lines) = running_tatolab.wait_for_exit();

    assert_eq!(exit_status.code(), Some(1), "{standard_error_lines:?}");
    assert_eq!(
        standard_error_lines.last().unwrap(),
        &format!(
            "error: the runtime closed the connection — it crashed or was stopped; its log is \
             under {}/",
            isolated_machine_directories
                .state_directory()
                .join("logs")
                .display()
        )
    );
    assert_eq!(calls_to(&stub_local_api_server, "stop_stream"), 0);
}

#[test]
fn a_refused_load_prints_the_refusal_and_exits_one() {
    let isolated_machine_directories = IsolatedMachineDirectories::new();
    let scratch_project = ScratchProject::new();
    let refusal = format!(
        "{0} has no .venv/bin/python; run `uv sync` in {0}",
        scratch_project.canonical_path().display()
    );
    let stub_local_api_server = a_runtime_holding(
        &isolated_machine_directories,
        vec![StubToolAnswer::tool_failure(&refusal)],
        Vec::new(),
    );

    let running_tatolab = RunningTatolab::spawn(isolated_machine_directories.tatolab_command(&[
        "run",
        "--dir",
        scratch_project.path_as_given(),
    ]));
    let (exit_status, standard_error_lines) = running_tatolab.wait_for_exit();

    assert_eq!(exit_status.code(), Some(1), "{standard_error_lines:?}");
    assert_eq!(
        standard_error_lines,
        [format!("error: run_stream failed: {refusal}")]
    );
    assert_eq!(
        tool_calls_besides_logs(&stub_local_api_server),
        ["run_stream"]
    );
    assert_eq!(calls_to(&stub_local_api_server, "logs"), 0);
}

#[test]
fn a_stream_unloaded_elsewhere_ends_run_with_zero_saying_so() {
    let isolated_machine_directories = IsolatedMachineDirectories::new();
    let scratch_project = ScratchProject::new();
    let _stub_local_api_server =
        isolated_machine_directories.serve_stub_local_api(StubLocalApiScript {
            tool_answers_by_name: HashMap::from([
                (
                    "run_stream".to_owned(),
                    vec![loaded_answer(&scratch_project.canonical_path())],
                ),
                (
                    "logs".to_owned(),
                    vec![StubToolAnswer::tool_failure(
                        "no stream `cam` is loaded; this runtime holds none",
                    )],
                ),
            ]),
            ..StubLocalApiScript::default()
        });

    let running_tatolab = RunningTatolab::spawn(isolated_machine_directories.tatolab_command(&[
        "run",
        "--dir",
        scratch_project.path_as_given(),
    ]));
    let (exit_status, standard_error_lines) = running_tatolab.wait_for_exit();

    assert_eq!(exit_status.code(), Some(0), "{standard_error_lines:?}");
    assert!(
        standard_error_lines.iter().any(|standard_error_line| {
            standard_error_line.starts_with("tatolab: cam was unloaded by the runtime")
                && standard_error_line.contains("no stream `cam` is loaded")
        }),
        "{standard_error_lines:?}"
    );
}

#[test]
fn dev_loads_again_on_a_saved_edit_and_survives_a_refused_load() {
    let isolated_machine_directories = IsolatedMachineDirectories::new();
    let scratch_project = ScratchProject::new();
    let stub_local_api_server = a_runtime_holding(
        &isolated_machine_directories,
        vec![
            loaded_answer(&scratch_project.canonical_path()),
            StubToolAnswer::tool_failure("SyntaxError: invalid syntax (stream.py, line 1)"),
            loaded_answer(&scratch_project.canonical_path()),
        ],
        Vec::new(),
    );
    let mut running_tatolab =
        RunningTatolab::spawn(isolated_machine_directories.tatolab_command(&[
            "dev",
            "--dir",
            scratch_project.path_as_given(),
        ]));
    running_tatolab.wait_for_standard_error_line_containing("tatolab dev: cam loaded");

    scratch_project.save_stream_py("this is not python (\n");
    running_tatolab.wait_for_standard_error_line_containing(
        "tatolab dev: run_stream failed: SyntaxError: invalid syntax (stream.py, line 1)",
    );
    running_tatolab.wait_for_standard_error_line_containing("fix it and save again");
    assert!(
        running_tatolab.is_still_running(),
        "a refused load ends nothing"
    );

    scratch_project.save_stream_py("# the fixed version, longer than the first\n");
    running_tatolab.wait_for_standard_error_line_containing("tatolab dev: cam loaded");
    running_tatolab.send_signal(libc::SIGINT);
    let (exit_status, standard_error_lines) = running_tatolab.wait_for_exit();

    assert_eq!(exit_status.code(), Some(0), "{standard_error_lines:?}");
    assert_eq!(
        tool_calls_besides_logs(&stub_local_api_server),
        [
            "run_stream",
            "stop_stream",
            "run_stream",
            "run_stream",
            "stop_stream"
        ],
        "the edit stops the stream and loads it again; the refused load leaves nothing to stop"
    );
    for run_stream_arguments in stub_local_api_server.recorded_arguments_of("run_stream") {
        assert_eq!(run_stream_arguments["keep"], false);
    }
}

#[test]
fn dev_waits_for_the_runtime_after_losing_its_connection_and_loads_again() {
    let isolated_machine_directories = IsolatedMachineDirectories::new();
    let scratch_project = ScratchProject::new();
    let stub_local_api_server = a_runtime_holding(
        &isolated_machine_directories,
        vec![loaded_answer(&scratch_project.canonical_path())],
        Vec::new(),
    );
    let mut running_tatolab =
        RunningTatolab::spawn(isolated_machine_directories.tatolab_command(&[
            "dev",
            "--dir",
            scratch_project.path_as_given(),
        ]));
    running_tatolab.wait_for_standard_error_line_containing("tatolab dev: cam loaded");

    stub_local_api_server.close_every_mcp_stdio_connection();
    running_tatolab
        .wait_for_standard_error_line_containing("tatolab dev: the runtime closed the connection");
    running_tatolab.wait_for_standard_error_line_containing("tatolab dev: cam loaded");
    running_tatolab.send_signal(libc::SIGTERM);
    let (exit_status, standard_error_lines) = running_tatolab.wait_for_exit();

    assert_eq!(exit_status.code(), Some(0), "{standard_error_lines:?}");
    assert_eq!(calls_to(&stub_local_api_server, "run_stream"), 2);
    assert_eq!(
        stub_local_api_server
            .recorded_mcp_stdio_request_heads()
            .len(),
        2,
        "the second load rides a new connection"
    );
}
