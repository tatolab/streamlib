// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `tatolab run` attached and `tatolab dev` run as a user runs them, against a stub local API at
//! an isolated machine's fixed socket: the load over a `/mcp/stdio` connection of the verb's own,
//! the records followed by sequence number and printed as the runtime mirrors them, Ctrl-C
//! stopping the stream, the runtime closing the connection, a refused load, and `dev` loading
//! again on a saved edit and after the runtime drops it.

#![cfg(any(target_os = "linux", feature = "machine-directories-under-a-test-root"))]

mod common;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::json;

use common::isolated_machine_directories::IsolatedMachineDirectories;
use common::running_tatolab::{RunningTatolab, wait_until};
use common::runtime_log_line_fixtures::a_log_record;
use common::stub_local_api_server::{
    StubLocalApiScript, StubLocalApiServer, StubMcpStdioUpgradeAnswer, StubStreamLogRecords,
    StubToolAnswer, StubToolCallTransport, run_stream_tool_result_text,
    stop_stream_tool_result_text,
};

/// The stream every attached load here is loaded as.
const ATTACHED_STREAM_NAME: &str = "cam";

/// Far inside the 30 s an attached stream's connection waits on an unanswered request: a stop
/// signal that waited on one would not exit within it.
const STOP_SIGNAL_EXIT_BOUND: Duration = Duration::from_secs(5);

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
    loaded_answer_warning(project_directory, &[])
}

/// The load's answer, its compile having written `compile_warnings` to its standard error.
fn loaded_answer_warning(project_directory: &Path, compile_warnings: &[&str]) -> StubToolAnswer {
    StubToolAnswer::tool_result(&run_stream_tool_result_text(
        ATTACHED_STREAM_NAME,
        false,
        project_directory,
        3,
        compile_warnings,
    ))
}

/// A cross-floor warning block, as a compile writes it to its standard error.
const CROSS_FLOOR_WARNING_LINES: [&str; 3] = [
    "tatolab: the cross-floor check found 1 thing binding this app to one floor (Linux or macOS). The app starts anyway.",
    "  processors/effect.py:4: imports `cupy`",
    "      portable: tatolab.stream's GPU context",
];

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

/// Assert every call to `tool_name` so far rode a `/mcp/stdio` connection, not `POST /mcp`.
fn assert_every_call_rode_the_mcp_stdio_connection(
    stub_local_api_server: &StubLocalApiServer,
    tool_name: &str,
) {
    let recorded_transports = stub_local_api_server.recorded_transports_of(tool_name);
    assert!(!recorded_transports.is_empty(), "no `{tool_name}` call");
    assert!(
        recorded_transports
            .iter()
            .all(|transport| *transport == StubToolCallTransport::McpStdioConnection),
        "`{tool_name}` came over {recorded_transports:?}"
    );
}

/// Send `signal`, then wait for the exit, asserting it came within [`STOP_SIGNAL_EXIT_BOUND`].
fn signal_and_wait_for_a_prompt_exit(
    running_tatolab: RunningTatolab,
    signal: libc::c_int,
) -> (std::process::ExitStatus, Vec<String>) {
    let signalled_at = Instant::now();
    running_tatolab.send_signal(signal);
    let (exit_status, standard_error_lines) = running_tatolab.wait_for_exit();
    let took = signalled_at.elapsed();
    assert!(
        took < STOP_SIGNAL_EXIT_BOUND,
        "exited {took:?} after the signal: {standard_error_lines:?}"
    );
    (exit_status, standard_error_lines)
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
    assert_every_call_rode_the_mcp_stdio_connection(&stub_local_api_server, "run_stream");
    assert_every_call_rode_the_mcp_stdio_connection(&stub_local_api_server, "logs");
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

/// The compile's standard error reaches the user's terminal, not only the runtime's log: each
/// line on `tatolab`'s own stderr, ahead of the note that the stream loaded and of its records.
#[test]
fn run_and_dev_print_each_line_the_compile_wrote_before_the_loaded_note() {
    for verb in ["run", "dev"] {
        let isolated_machine_directories = IsolatedMachineDirectories::new();
        let scratch_project = ScratchProject::new();
        let _stub_local_api_server = a_runtime_holding(
            &isolated_machine_directories,
            vec![loaded_answer_warning(
                &scratch_project.canonical_path(),
                &CROSS_FLOOR_WARNING_LINES,
            )],
            vec![a_log_record(json!({"message": "first"}))],
        );
        let running_tatolab =
            RunningTatolab::spawn(isolated_machine_directories.tatolab_command(&[
                verb,
                "--dir",
                scratch_project.path_as_given(),
            ]));
        running_tatolab.next_standard_output_line("the first record");
        running_tatolab.send_signal(libc::SIGINT);
        let (exit_status, standard_error_lines) = running_tatolab.wait_for_exit();

        assert_eq!(
            exit_status.code(),
            Some(0),
            "{verb}: {standard_error_lines:?}"
        );
        let loaded_note_index = standard_error_lines
            .iter()
            .position(|standard_error_line| standard_error_line.contains("cam loaded (3 nodes"))
            .unwrap_or_else(|| panic!("{verb} noted no load: {standard_error_lines:?}"));
        assert_eq!(
            standard_error_lines[..loaded_note_index],
            CROSS_FLOOR_WARNING_LINES,
            "{verb}: the compile's lines, verbatim and in order, ahead of the loaded note"
        );
    }
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
    assert_every_call_rode_the_mcp_stdio_connection(&stub_local_api_server, "stop_stream");
    assert!(
        standard_error_lines
            .iter()
            .any(|standard_error_line| standard_error_line == "tatolab: cam stopped"),
        "{standard_error_lines:?}"
    );
}

#[test]
fn ctrl_c_before_the_runtime_opens_the_connection_exits_zero_at_once() {
    let isolated_machine_directories = IsolatedMachineDirectories::new();
    let scratch_project = ScratchProject::new();
    let stub_local_api_server =
        isolated_machine_directories.serve_stub_local_api(StubLocalApiScript {
            mcp_stdio_upgrade_answer: StubMcpStdioUpgradeAnswer::NeverAnswer,
            ..StubLocalApiScript::default()
        });
    let running_tatolab = RunningTatolab::spawn(isolated_machine_directories.tatolab_command(&[
        "run",
        "--dir",
        scratch_project.path_as_given(),
    ]));
    wait_until("the /mcp/stdio upgrade request", || {
        !stub_local_api_server
            .recorded_mcp_stdio_request_heads()
            .is_empty()
    });

    let (exit_status, standard_error_lines) =
        signal_and_wait_for_a_prompt_exit(running_tatolab, libc::SIGINT);

    assert_eq!(exit_status.code(), Some(0), "{standard_error_lines:?}");
    assert_eq!(
        standard_error_lines,
        ["tatolab: stopped before the runtime opened a connection; nothing was loaded"]
    );
    assert!(stub_local_api_server.recorded_tool_calls().is_empty());
}

#[test]
fn a_second_ctrl_c_leaves_without_waiting_for_a_stop_the_runtime_does_not_answer() {
    let isolated_machine_directories = IsolatedMachineDirectories::new();
    let scratch_project = ScratchProject::new();
    let stub_local_api_server =
        isolated_machine_directories.serve_stub_local_api(StubLocalApiScript {
            tool_answers_by_name: HashMap::from([(
                "run_stream".to_owned(),
                vec![loaded_answer(&scratch_project.canonical_path())],
            )]),
            stream_log_records: Some(StubStreamLogRecords {
                records: vec![a_log_record(json!({"message": "running"}))],
                page_size: 1,
            }),
            tools_that_never_answer: HashSet::from(["stop_stream".to_owned()]),
            ..StubLocalApiScript::default()
        });
    let running_tatolab = RunningTatolab::spawn(isolated_machine_directories.tatolab_command(&[
        "run",
        "--dir",
        scratch_project.path_as_given(),
    ]));
    running_tatolab.next_standard_output_line("the stream's record");
    running_tatolab.send_signal(libc::SIGINT);
    wait_until("the `stop_stream` call", || {
        calls_to(&stub_local_api_server, "stop_stream") == 1
    });

    let (exit_status, standard_error_lines) =
        signal_and_wait_for_a_prompt_exit(running_tatolab, libc::SIGINT);

    assert_eq!(exit_status.code(), Some(0), "{standard_error_lines:?}");
    assert_eq!(
        standard_error_lines.last().unwrap(),
        "tatolab: stopped; the runtime unloads cam as this connection drops"
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

/// A save runs the stream again over the same connection, never stopping it first: the runtime
/// replaces the running stream once the save compiles, so a save it refuses leaves that stream
/// running, and `dev` follows it on from the record it had reached.
#[test]
fn dev_runs_again_on_a_saved_edit_without_stopping_and_follows_on_after_a_refused_save() {
    let isolated_machine_directories = IsolatedMachineDirectories::new();
    let scratch_project = ScratchProject::new();
    let stub_local_api_server = a_runtime_holding(
        &isolated_machine_directories,
        vec![
            loaded_answer(&scratch_project.canonical_path()),
            StubToolAnswer::tool_failure("SyntaxError: invalid syntax (stream.py, line 1)"),
            loaded_answer(&scratch_project.canonical_path()),
        ],
        vec![a_log_record(json!({"message": "before the bad save"}))],
    );
    let mut running_tatolab =
        RunningTatolab::spawn(isolated_machine_directories.tatolab_command(&[
            "dev",
            "--dir",
            scratch_project.path_as_given(),
        ]));
    running_tatolab.wait_for_standard_error_line_containing("tatolab dev: cam loaded");
    assert_eq!(
        running_tatolab.next_standard_output_line("the record before the bad save"),
        rendered_line("before the bad save")
    );

    scratch_project.save_stream_py("this is not python (\n");
    running_tatolab.wait_for_standard_error_line_containing(
        "tatolab dev: run_stream failed: SyntaxError: invalid syntax (stream.py, line 1)",
    );
    running_tatolab.wait_for_standard_error_line_containing(
        "tatolab dev: cam keeps running the last save that loaded — fix it and save again",
    );
    stub_local_api_server
        .append_stream_log_records(vec![a_log_record(json!({"message": "after the bad save"}))]);
    assert_eq!(
        running_tatolab.next_standard_output_line("the record after the bad save"),
        rendered_line("after the bad save"),
        "the stream running before the bad save is followed on, past the record already printed"
    );
    assert!(
        running_tatolab.is_still_running(),
        "a refused save ends nothing"
    );

    scratch_project.save_stream_py("# the fixed version, longer than the first\n");
    wait_until("the run after the fixed save", || {
        calls_to(&stub_local_api_server, "run_stream") == 3
    });
    running_tatolab.wait_for_standard_error_line_containing("tatolab dev: cam loaded");
    running_tatolab.send_signal(libc::SIGINT);
    let (exit_status, standard_error_lines) = running_tatolab.wait_for_exit();

    assert_eq!(exit_status.code(), Some(0), "{standard_error_lines:?}");
    assert_eq!(
        tool_calls_besides_logs(&stub_local_api_server),
        ["run_stream", "run_stream", "run_stream", "stop_stream"],
        "each save runs the stream again over the connection; only Ctrl-C stops it"
    );
    assert!(
        !standard_error_lines
            .iter()
            .any(|standard_error_line| standard_error_line.contains("no stream is loaded")),
        "{standard_error_lines:?}"
    );
    for run_stream_arguments in stub_local_api_server.recorded_arguments_of("run_stream") {
        assert_eq!(run_stream_arguments["keep"], false);
    }
    assert_eq!(
        stub_local_api_server
            .recorded_mcp_stdio_request_heads()
            .len(),
        1,
        "every run rides the one connection that attached the stream"
    );
    assert_every_call_rode_the_mcp_stdio_connection(&stub_local_api_server, "run_stream");
    assert_every_call_rode_the_mcp_stdio_connection(&stub_local_api_server, "stop_stream");
}

/// A save that compiles and whose load the runtime refuses leaves the previous graph loaded again
/// as another instance, numbering its records from 1: `dev` follows that instance from its first
/// record, not from where it had read the stream the save was to replace.
#[test]
fn dev_follows_the_stream_loaded_again_after_a_refused_save_that_compiled_from_its_first_record() {
    let isolated_machine_directories = IsolatedMachineDirectories::new();
    let scratch_project = ScratchProject::new();
    let stub_local_api_server = a_runtime_holding(
        &isolated_machine_directories,
        vec![
            loaded_answer(&scratch_project.canonical_path()),
            StubToolAnswer::tool_failure(
                "the stream `cam` was not loaded: no output port `absent-port` on node `source`",
            )
            .re_loading_the_stream_as(
                "2",
                vec![a_log_record(json!({"message": "the re-load starts"}))],
            ),
        ],
        vec![
            a_log_record(json!({"message": "first"})),
            a_log_record(json!({"message": "second"})),
            a_log_record(json!({"message": "third"})),
        ],
    );
    let mut running_tatolab =
        RunningTatolab::spawn(isolated_machine_directories.tatolab_command(&[
            "dev",
            "--dir",
            scratch_project.path_as_given(),
        ]));
    for message in ["first", "second", "third"] {
        assert_eq!(
            running_tatolab.next_standard_output_line(message),
            rendered_line(message)
        );
    }

    scratch_project.save_stream_py(
        "# a save whose load is refused, longer than the first
",
    );
    running_tatolab.wait_for_standard_error_line_containing(
        "tatolab dev: run_stream failed: the stream `cam` was not loaded",
    );
    running_tatolab.wait_for_standard_error_line_containing(
        "tatolab dev: cam was loaded again — following it from its first record",
    );

    assert_eq!(
        running_tatolab.next_standard_output_line("the re-loaded stream's first record"),
        rendered_line("the re-load starts"),
        "the re-loaded stream's records are followed from its first"
    );
    running_tatolab.wait_for_standard_error_line_containing(
        "tatolab dev: cam keeps running the last save that loaded — fix it and save again",
    );
    let (exit_status, standard_error_lines) =
        signal_and_wait_for_a_prompt_exit(running_tatolab, libc::SIGINT);
    assert_eq!(exit_status.code(), Some(0), "{standard_error_lines:?}");
    assert_eq!(
        tool_calls_besides_logs(&stub_local_api_server),
        ["run_stream", "run_stream", "stop_stream"]
    );
}

/// A `logs` page naming another instance of the followed stream than the one followed is a
/// re-load: the follow sets that page aside and reads the new instance from its first record.
#[test]
fn run_follows_a_stream_loaded_again_mid_follow_from_its_first_record() {
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
            "--dir",
            scratch_project.path_as_given(),
        ]));
    for message in ["first", "second"] {
        assert_eq!(
            running_tatolab.next_standard_output_line(message),
            rendered_line(message)
        );
    }

    stub_local_api_server.re_load_the_stream_as(
        "2",
        vec![a_log_record(json!({"message": "after the re-load"}))],
    );

    running_tatolab.wait_for_standard_error_line_containing(
        "tatolab: cam was loaded again — following it from its first record",
    );
    assert_eq!(
        running_tatolab.next_standard_output_line("the re-loaded stream's first record"),
        rendered_line("after the re-load")
    );
    let (exit_status, standard_error_lines) =
        signal_and_wait_for_a_prompt_exit(running_tatolab, libc::SIGINT);
    assert_eq!(exit_status.code(), Some(0), "{standard_error_lines:?}");
}

/// A one-node stream's loaded note counts one node.
#[test]
fn the_loaded_note_counts_a_single_node_in_the_singular() {
    let isolated_machine_directories = IsolatedMachineDirectories::new();
    let scratch_project = ScratchProject::new();
    let _stub_local_api_server = a_runtime_holding(
        &isolated_machine_directories,
        vec![StubToolAnswer::tool_result(&run_stream_tool_result_text(
            ATTACHED_STREAM_NAME,
            false,
            &scratch_project.canonical_path(),
            1,
            &[],
        ))],
        Vec::new(),
    );
    let mut running_tatolab =
        RunningTatolab::spawn(isolated_machine_directories.tatolab_command(&[
            "run",
            "--dir",
            scratch_project.path_as_given(),
        ]));

    running_tatolab.wait_for_standard_error_line_containing(&format!(
        "tatolab: cam loaded (1 node, project {})",
        scratch_project.canonical_path().display()
    ));
    drop(running_tatolab);
}

#[test]
fn dev_waits_for_a_save_when_the_runtime_answers_logs_with_a_page_it_cannot_read() {
    let isolated_machine_directories = IsolatedMachineDirectories::new();
    let scratch_project = ScratchProject::new();
    let stub_local_api_server =
        isolated_machine_directories.serve_stub_local_api(StubLocalApiScript {
            tool_answers_by_name: HashMap::from([
                (
                    "run_stream".to_owned(),
                    vec![loaded_answer(&scratch_project.canonical_path())],
                ),
                ("stop_stream".to_owned(), vec![stopped_answer()]),
                (
                    "logs".to_owned(),
                    vec![StubToolAnswer::tool_result(r#"{"stream": "cam"}"#)],
                ),
            ]),
            ..StubLocalApiScript::default()
        });
    let mut running_tatolab =
        RunningTatolab::spawn(isolated_machine_directories.tatolab_command(&[
            "dev",
            "--dir",
            scratch_project.path_as_given(),
        ]));
    running_tatolab.wait_for_standard_error_line_containing("save again to load it");
    // Ten of `dev`'s reconnect polls: a page it cannot read must not drive a load loop.
    std::thread::sleep(Duration::from_millis(2500));

    assert!(running_tatolab.is_still_running());
    assert_eq!(
        tool_calls_besides_logs(&stub_local_api_server),
        ["run_stream", "stop_stream"],
        "the unreadable page stops the stream and waits for a save"
    );
    assert_eq!(calls_to(&stub_local_api_server, "logs"), 1);
    assert_eq!(
        stub_local_api_server
            .recorded_mcp_stdio_request_heads()
            .len(),
        1
    );

    scratch_project.save_stream_py("# the edited version, longer than the first\n");
    wait_until("the load after the save", || {
        calls_to(&stub_local_api_server, "run_stream") == 2
    });
    let (exit_status, standard_error_lines) =
        signal_and_wait_for_a_prompt_exit(running_tatolab, libc::SIGINT);
    assert_eq!(exit_status.code(), Some(0), "{standard_error_lines:?}");
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

#[test]
fn ctrl_c_while_dev_reconnects_to_a_runtime_that_never_opens_the_connection_exits_zero() {
    let isolated_machine_directories = IsolatedMachineDirectories::new();
    let scratch_project = ScratchProject::new();
    let first_stub_local_api_server = a_runtime_holding(
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

    drop(first_stub_local_api_server);
    running_tatolab.wait_for_standard_error_line_containing("waiting for a runtime to answer");
    let unanswering_stub_local_api_server =
        isolated_machine_directories.serve_stub_local_api(StubLocalApiScript {
            mcp_stdio_upgrade_answer: StubMcpStdioUpgradeAnswer::NeverAnswer,
            ..StubLocalApiScript::default()
        });
    wait_until("the reconnect's /mcp/stdio upgrade request", || {
        !unanswering_stub_local_api_server
            .recorded_mcp_stdio_request_heads()
            .is_empty()
    });

    let (exit_status, standard_error_lines) =
        signal_and_wait_for_a_prompt_exit(running_tatolab, libc::SIGINT);

    assert_eq!(exit_status.code(), Some(0), "{standard_error_lines:?}");
}

#[test]
fn ctrl_c_while_dev_stops_a_stream_whose_logs_it_cannot_read_exits_zero() {
    let isolated_machine_directories = IsolatedMachineDirectories::new();
    let scratch_project = ScratchProject::new();
    let stub_local_api_server =
        isolated_machine_directories.serve_stub_local_api(StubLocalApiScript {
            tool_answers_by_name: HashMap::from([
                (
                    "run_stream".to_owned(),
                    vec![loaded_answer(&scratch_project.canonical_path())],
                ),
                (
                    "logs".to_owned(),
                    vec![StubToolAnswer::tool_result(r#"{"stream": "cam"}"#)],
                ),
            ]),
            tools_that_never_answer: HashSet::from(["stop_stream".to_owned()]),
            ..StubLocalApiScript::default()
        });
    let running_tatolab = RunningTatolab::spawn(isolated_machine_directories.tatolab_command(&[
        "dev",
        "--dir",
        scratch_project.path_as_given(),
    ]));
    wait_until("the `stop_stream` call", || {
        calls_to(&stub_local_api_server, "stop_stream") == 1
    });

    let (exit_status, standard_error_lines) =
        signal_and_wait_for_a_prompt_exit(running_tatolab, libc::SIGINT);

    assert_eq!(exit_status.code(), Some(0), "{standard_error_lines:?}");
    assert_eq!(
        standard_error_lines.last().unwrap(),
        "tatolab dev: stopped; the runtime unloads cam as this connection drops"
    );
}
