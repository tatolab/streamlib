// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `tatolab dev` and `tatolab logs -f` run as a user runs them, against a stub local API at an
//! isolated machine's fixed socket: `dev` loads over a `/mcp/stdio` connection of its own,
//! follows the stream's records and runs it again on a saved edit; Ctrl-C ends either verb and
//! never stops the stream; a runtime that goes away ends `logs -f` naming its log, while `dev`
//! waits for it and follows the re-loaded stream again without loading it.

#![cfg(any(target_os = "linux", feature = "machine-directories-under-a-test-root"))]

mod common;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::json;

use common::isolated_machine_directories::IsolatedMachineDirectories;
use common::running_tatolab::{RunningTatolab, wait_until};
use common::runtime_log_line_fixtures::a_log_record;
use common::stub_local_api_server::{
    StubLocalApiScript, StubLocalApiServer, StubMcpStdioUpgradeAnswer, StubStreamLogRecords,
    StubToolAnswer, StubToolCallTransport, failed_run_stream_tool_result_text,
    run_stream_tool_result_text,
};

/// The stream every load here is loaded as.
const STREAM_ID: &str = "cam";

/// Far inside the 30 s `dev`'s connection waits on an unanswered request: a stop signal that
/// waited on one would not exit within it.
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
        STREAM_ID,
        project_directory,
        3,
        compile_warnings,
    ))
}

/// `dev`'s note once the stream loaded, for `project_directory`.
fn running_note(project_directory: &Path) -> String {
    format!(
        "tatolab dev: cam running as \"main\" (project {}, stream:main)",
        project_directory.display()
    )
}

/// A cross-floor warning block, as a compile writes it to its standard error.
const CROSS_FLOOR_WARNING_LINES: [&str; 3] = [
    "tatolab: the cross-floor check found 1 thing binding this app to one floor (Linux or macOS). The app starts anyway.",
    "  processors/effect.py:4: imports `cupy`",
    "      portable: tatolab.stream's GPU context",
];

/// `list_streams`' answer: `cam`, loaded from `project_directory`, running with three nodes.
fn the_stream_listed_running_from(project_directory: &Path) -> StubToolAnswer {
    StubToolAnswer::tool_result(
        &json!({"streams": [{
            "stream": STREAM_ID, "display_name": "main", "state": "running",
            "location": {"project_directory": project_directory, "stream_function": "stream:main"},
            "node_count": 3, "failed_because": null,
        }]})
        .to_string(),
    )
}

/// `describe_runtime`'s answer: a runtime writing its own log in `runtime_log_directory`.
fn a_runtime_logging_in(runtime_log_directory: &str) -> StubToolAnswer {
    StubToolAnswer::tool_result(
        &json!({
            "version": "0.57.0",
            "build_id": "0.57.0+0123abc.9f8e7d",
            "up_for_ms": 5,
            "runtime_log_directory": runtime_log_directory,
        })
        .to_string(),
    )
}

/// A runtime that loads the stream as `run_stream_answers` say, in order, lists it running, and
/// pages `stream_log_records` one record per `logs` call.
fn a_runtime_holding(
    isolated_machine_directories: &IsolatedMachineDirectories,
    project_directory: &Path,
    run_stream_answers: Vec<StubToolAnswer>,
    stream_log_records: Vec<serde_json::Value>,
) -> StubLocalApiServer {
    isolated_machine_directories.serve_stub_local_api(StubLocalApiScript {
        tool_answers_by_name: HashMap::from([
            ("run_stream".to_owned(), run_stream_answers),
            (
                "list_streams".to_owned(),
                vec![the_stream_listed_running_from(project_directory)],
            ),
        ]),
        stream_log_records: Some(StubStreamLogRecords {
            records: stream_log_records,
            page_size: 1,
        }),
        ..StubLocalApiScript::default()
    })
}

/// The tools called, in order, less the `logs` polls and the runtime's description.
fn tool_calls_besides_logs(stub_local_api_server: &StubLocalApiServer) -> Vec<String> {
    stub_local_api_server
        .recorded_tool_calls()
        .into_iter()
        .map(|recorded_tool_call| recorded_tool_call.tool_name)
        .filter(|tool_name| tool_name != "logs" && tool_name != "describe_runtime")
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

fn spawn_dev(
    isolated_machine_directories: &IsolatedMachineDirectories,
    scratch_project: &ScratchProject,
) -> RunningTatolab {
    RunningTatolab::spawn(isolated_machine_directories.tatolab_command(&[
        "dev",
        "--dir",
        scratch_project.path_as_given(),
    ]))
}

#[test]
fn dev_loads_the_stream_and_prints_each_record_as_the_runtime_mirrors_it() {
    let isolated_machine_directories = IsolatedMachineDirectories::new();
    let scratch_project = ScratchProject::new();
    let stub_local_api_server = a_runtime_holding(
        &isolated_machine_directories,
        &scratch_project.canonical_path(),
        vec![loaded_answer(&scratch_project.canonical_path())],
        vec![
            a_log_record(json!({"message": "first"})),
            a_log_record(json!({"message": "second"})),
        ],
    );

    let mut running_tatolab =
        RunningTatolab::spawn(isolated_machine_directories.tatolab_command(&[
            "dev",
            "stream.py:main",
            "--dir",
            scratch_project.path_as_given(),
            "--name",
            "Desk camera",
        ]));

    assert_eq!(
        running_tatolab.next_standard_output_line("the first record"),
        rendered_line("first")
    );
    assert_eq!(
        running_tatolab.next_standard_output_line("the second record"),
        rendered_line("second")
    );
    running_tatolab
        .wait_for_standard_error_line_containing(&running_note(&scratch_project.canonical_path()));
    wait_until("a third `logs` call", || {
        calls_to(&stub_local_api_server, "logs") >= 3
    });
    assert_eq!(
        stub_local_api_server.recorded_arguments_of("run_stream"),
        [json!({
            "project_directory": scratch_project.canonical_path(),
            "stream_function": "stream.py:main",
            "name": "Desk camera",
        })]
    );
    assert_eq!(
        stub_local_api_server
            .recorded_mcp_stdio_request_heads()
            .len(),
        1,
        "dev rides one /mcp/stdio connection of its own"
    );
    assert_every_call_rode_the_mcp_stdio_connection(&stub_local_api_server, "run_stream");
    assert_every_call_rode_the_mcp_stdio_connection(&stub_local_api_server, "logs");
    let logs_afters: Vec<u64> = stub_local_api_server
        .recorded_arguments_of("logs")
        .iter()
        .map(|logs_arguments| {
            assert_eq!(logs_arguments["stream"], STREAM_ID);
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

/// The compile's standard error reaches the user's terminal: each line on `dev`'s own stderr,
/// ahead of the note that the stream is running and of its records.
#[test]
fn dev_prints_each_line_the_compile_wrote_before_the_running_note() {
    let isolated_machine_directories = IsolatedMachineDirectories::new();
    let scratch_project = ScratchProject::new();
    let _stub_local_api_server = a_runtime_holding(
        &isolated_machine_directories,
        &scratch_project.canonical_path(),
        vec![loaded_answer_warning(
            &scratch_project.canonical_path(),
            &CROSS_FLOOR_WARNING_LINES,
        )],
        vec![a_log_record(json!({"message": "first"}))],
    );
    let running_tatolab = spawn_dev(&isolated_machine_directories, &scratch_project);
    running_tatolab.next_standard_output_line("the first record");

    let (exit_status, standard_error_lines) =
        signal_and_wait_for_a_prompt_exit(running_tatolab, libc::SIGINT);

    assert_eq!(exit_status.code(), Some(0), "{standard_error_lines:?}");
    let running_note_index = standard_error_lines
        .iter()
        .position(|standard_error_line| {
            *standard_error_line == running_note(&scratch_project.canonical_path())
        })
        .unwrap_or_else(|| panic!("dev noted no load: {standard_error_lines:?}"));
    assert_eq!(
        standard_error_lines[..running_note_index],
        CROSS_FLOOR_WARNING_LINES,
        "the compile's lines, verbatim and in order, ahead of the running note"
    );
}

/// Ctrl-C ends `dev`'s following and leaves the stream loaded: no `stop_stream`, exit 0.
#[test]
fn ctrl_c_on_dev_ends_the_following_and_never_stops_the_stream() {
    for stop_signal in [libc::SIGINT, libc::SIGTERM] {
        let isolated_machine_directories = IsolatedMachineDirectories::new();
        let scratch_project = ScratchProject::new();
        let stub_local_api_server = a_runtime_holding(
            &isolated_machine_directories,
            &scratch_project.canonical_path(),
            vec![loaded_answer(&scratch_project.canonical_path())],
            vec![a_log_record(json!({"message": "running"}))],
        );
        let running_tatolab = spawn_dev(&isolated_machine_directories, &scratch_project);
        running_tatolab.next_standard_output_line("the stream's record");

        let (exit_status, standard_error_lines) =
            signal_and_wait_for_a_prompt_exit(running_tatolab, stop_signal);

        assert_eq!(exit_status.code(), Some(0), "{standard_error_lines:?}");
        assert_eq!(calls_to(&stub_local_api_server, "stop_stream"), 0);
        assert_eq!(
            tool_calls_besides_logs(&stub_local_api_server),
            ["run_stream"]
        );
        assert_eq!(
            standard_error_lines.last().unwrap(),
            "tatolab dev: stopped following; cam keeps running — `tatolab stop cam` stops it"
        );
    }
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
    let running_tatolab = spawn_dev(&isolated_machine_directories, &scratch_project);
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
        ["tatolab dev: stopped before the runtime opened a connection; nothing was loaded"]
    );
    assert!(stub_local_api_server.recorded_tool_calls().is_empty());
}

#[test]
fn a_refused_first_load_waits_for_a_save_and_loads_again_on_it() {
    let isolated_machine_directories = IsolatedMachineDirectories::new();
    let scratch_project = ScratchProject::new();
    let refusal = format!(
        "`{0}/.venv/bin/python` cannot import `tatolab.stream`: add `tatolab-stream` to the \
         project's dependencies and run `uv sync` in {0}.",
        scratch_project.canonical_path().display()
    );
    let stub_local_api_server = a_runtime_holding(
        &isolated_machine_directories,
        &scratch_project.canonical_path(),
        vec![
            StubToolAnswer::tool_failure(&refusal),
            loaded_answer(&scratch_project.canonical_path()),
        ],
        vec![a_log_record(json!({"message": "loaded at last"}))],
    );
    let mut running_tatolab = spawn_dev(&isolated_machine_directories, &scratch_project);
    running_tatolab.wait_for_standard_error_line_containing(&format!(
        "tatolab dev: run_stream failed: {refusal}"
    ));
    running_tatolab.wait_for_standard_error_line_containing(
        "tatolab dev: no stream is loaded — fix it and save again",
    );

    scratch_project.save_stream_py("# the fixed version, longer than the first\n");

    assert_eq!(
        running_tatolab.next_standard_output_line("the record of the load after the save"),
        rendered_line("loaded at last")
    );
    let (exit_status, standard_error_lines) =
        signal_and_wait_for_a_prompt_exit(running_tatolab, libc::SIGINT);
    assert_eq!(exit_status.code(), Some(0), "{standard_error_lines:?}");
    assert_eq!(
        tool_calls_besides_logs(&stub_local_api_server),
        ["run_stream", "run_stream"]
    );
}

/// A save runs the stream again over the same connection, never stopping it first: a save the
/// runtime refuses leaves the running stream as it was, and `dev` follows it on from the record
/// it had reached.
#[test]
fn dev_runs_again_on_a_saved_edit_without_stopping_and_follows_on_after_a_refused_save() {
    let isolated_machine_directories = IsolatedMachineDirectories::new();
    let scratch_project = ScratchProject::new();
    let stub_local_api_server = a_runtime_holding(
        &isolated_machine_directories,
        &scratch_project.canonical_path(),
        vec![
            loaded_answer(&scratch_project.canonical_path()),
            StubToolAnswer::tool_failure(
                "the project's `tatolab-stream` is older than this runtime",
            ),
            loaded_answer(&scratch_project.canonical_path()),
        ],
        vec![a_log_record(json!({"message": "before the bad save"}))],
    );
    let mut running_tatolab = spawn_dev(&isolated_machine_directories, &scratch_project);
    running_tatolab
        .wait_for_standard_error_line_containing(&running_note(&scratch_project.canonical_path()));
    assert_eq!(
        running_tatolab.next_standard_output_line("the record before the bad save"),
        rendered_line("before the bad save")
    );

    scratch_project.save_stream_py("this is not python (\n");
    running_tatolab.wait_for_standard_error_line_containing(
        "tatolab dev: run_stream failed: the project's `tatolab-stream` is older than this runtime",
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
    running_tatolab
        .wait_for_standard_error_line_containing(&running_note(&scratch_project.canonical_path()));
    let (exit_status, standard_error_lines) =
        signal_and_wait_for_a_prompt_exit(running_tatolab, libc::SIGINT);

    assert_eq!(exit_status.code(), Some(0), "{standard_error_lines:?}");
    assert_eq!(
        tool_calls_besides_logs(&stub_local_api_server),
        ["run_stream", "run_stream", "run_stream"],
        "each save runs the stream again over the connection; nothing stops it"
    );
    assert_eq!(
        stub_local_api_server
            .recorded_mcp_stdio_request_heads()
            .len(),
        1,
        "every run rides the one connection"
    );
    assert_every_call_rode_the_mcp_stdio_connection(&stub_local_api_server, "run_stream");
}

/// A save whose code fails comes back as the stream failed: `dev` prints its traceback, says
/// it waits for a fix, and loads the stream again on the next save.
#[test]
fn dev_prints_a_failed_saves_traceback_and_loads_again_on_the_fixed_save() {
    let isolated_machine_directories = IsolatedMachineDirectories::new();
    let scratch_project = ScratchProject::new();
    let traceback = "Traceback (most recent call last):\n  File \"stream.py\", line 6, in main\n\
                     ValueError: bad wiring";
    let stub_local_api_server = a_runtime_holding(
        &isolated_machine_directories,
        &scratch_project.canonical_path(),
        vec![
            loaded_answer(&scratch_project.canonical_path()),
            StubToolAnswer::tool_result(&failed_run_stream_tool_result_text(
                STREAM_ID,
                &scratch_project.canonical_path(),
                traceback,
            )),
            loaded_answer(&scratch_project.canonical_path()),
        ],
        vec![a_log_record(json!({"message": "before the bad save"}))],
    );
    let mut running_tatolab = spawn_dev(&isolated_machine_directories, &scratch_project);
    assert_eq!(
        running_tatolab.next_standard_output_line("the record before the bad save"),
        rendered_line("before the bad save")
    );

    scratch_project.save_stream_py("# a save whose function raises, longer than the first\n");
    running_tatolab.wait_for_standard_error_line_containing(
        "tatolab dev: cam failed: Traceback (most recent call last):",
    );
    running_tatolab.wait_for_standard_error_line_containing("ValueError: bad wiring");
    running_tatolab.wait_for_standard_error_line_containing(
        "tatolab dev: cam is failed — fix it and save again to load it",
    );
    assert!(
        running_tatolab.is_still_running(),
        "a failed save ends nothing"
    );

    scratch_project.save_stream_py("# the fixed version, longer than every save before it\n");
    wait_until("the run after the fixed save", || {
        calls_to(&stub_local_api_server, "run_stream") == 3
    });
    running_tatolab
        .wait_for_standard_error_line_containing(&running_note(&scratch_project.canonical_path()));
    let (exit_status, standard_error_lines) =
        signal_and_wait_for_a_prompt_exit(running_tatolab, libc::SIGINT);
    assert_eq!(exit_status.code(), Some(0), "{standard_error_lines:?}");
    assert_eq!(
        tool_calls_besides_logs(&stub_local_api_server),
        ["run_stream", "run_stream", "run_stream"]
    );
}

/// A page `dev` cannot read leaves the stream running unfollowed, and waits for a save rather
/// than stopping the stream or driving a load loop.
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
                (
                    "logs".to_owned(),
                    vec![StubToolAnswer::tool_result(r#"{"stream": "cam"}"#)],
                ),
            ]),
            ..StubLocalApiScript::default()
        });
    let mut running_tatolab = spawn_dev(&isolated_machine_directories, &scratch_project);
    running_tatolab.wait_for_standard_error_line_containing("save again to load it");
    // Ten of `dev`'s runtime polls: a page it cannot read must not drive a load loop.
    std::thread::sleep(Duration::from_millis(2500));

    assert!(running_tatolab.is_still_running());
    assert_eq!(
        tool_calls_besides_logs(&stub_local_api_server),
        ["run_stream"]
    );
    assert_eq!(calls_to(&stub_local_api_server, "logs"), 1);

    scratch_project.save_stream_py("# the edited version, longer than the first\n");
    wait_until("the load after the save", || {
        calls_to(&stub_local_api_server, "run_stream") == 2
    });
    let (exit_status, standard_error_lines) =
        signal_and_wait_for_a_prompt_exit(running_tatolab, libc::SIGINT);
    assert_eq!(exit_status.code(), Some(0), "{standard_error_lines:?}");
    assert_eq!(calls_to(&stub_local_api_server, "stop_stream"), 0);
}

/// The runtime closing `dev`'s connection is a crash: `dev` names it and the log folder the
/// runtime described, waits for the runtime, and follows the stream once the runtime has re-loaded
/// it from its record — never sending `run_stream` for it.
#[test]
fn dev_survives_a_dropped_runtime_and_follows_the_re_loaded_stream_without_loading_it() {
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
                    "describe_runtime".to_owned(),
                    vec![a_runtime_logging_in("/home/u/.local/state/tatolab/logs")],
                ),
                (
                    "list_streams".to_owned(),
                    vec![the_stream_listed_running_from(
                        &scratch_project.canonical_path(),
                    )],
                ),
            ]),
            stream_log_records: Some(StubStreamLogRecords {
                records: vec![a_log_record(json!({"message": "before the crash"}))],
                page_size: 1,
            }),
            ..StubLocalApiScript::default()
        });
    let mut running_tatolab = spawn_dev(&isolated_machine_directories, &scratch_project);
    assert_eq!(
        running_tatolab.next_standard_output_line("the record before the crash"),
        rendered_line("before the crash")
    );

    stub_local_api_server.re_load_the_stream_as(
        "2",
        vec![a_log_record(json!({"message": "after the re-load"}))],
    );
    stub_local_api_server.close_every_mcp_stdio_connection();
    running_tatolab.wait_for_standard_error_line_containing(
        "tatolab dev: the runtime went away — it crashed or was stopped; its log is under \
         /home/u/.local/state/tatolab/logs/",
    );
    running_tatolab
        .wait_for_standard_error_line_containing("tatolab dev: cam is loaded again — following it");
    assert_eq!(
        running_tatolab.next_standard_output_line("the re-loaded stream's first record"),
        rendered_line("after the re-load")
    );
    let (exit_status, standard_error_lines) =
        signal_and_wait_for_a_prompt_exit(running_tatolab, libc::SIGINT);

    assert_eq!(exit_status.code(), Some(0), "{standard_error_lines:?}");
    assert_eq!(
        calls_to(&stub_local_api_server, "run_stream"),
        1,
        "the crash never sends `run_stream` again"
    );
    assert_eq!(calls_to(&stub_local_api_server, "stop_stream"), 0);
    assert_eq!(
        stub_local_api_server
            .recorded_mcp_stdio_request_heads()
            .len(),
        2,
        "the follow after the crash rides a new connection"
    );
}

/// A runtime that comes back holding the stream not yet re-loaded is asked again until it is.
#[test]
fn dev_waits_for_the_restarted_runtime_to_re_load_the_stream_before_following_it() {
    let isolated_machine_directories = IsolatedMachineDirectories::new();
    let scratch_project = ScratchProject::new();
    let first_runtime = a_runtime_holding(
        &isolated_machine_directories,
        &scratch_project.canonical_path(),
        vec![loaded_answer(&scratch_project.canonical_path())],
        vec![a_log_record(json!({"message": "before the crash"}))],
    );
    let mut running_tatolab = spawn_dev(&isolated_machine_directories, &scratch_project);
    running_tatolab.next_standard_output_line("the record before the crash");

    drop(first_runtime);
    running_tatolab.wait_for_standard_error_line_containing("tatolab dev: the runtime went away");
    running_tatolab.wait_for_standard_error_line_containing("waiting for a runtime to answer");
    let not_yet_re_loaded = json!({"streams": [{
        "stream": STREAM_ID, "display_name": "main", "state": "running",
        "location": {"project_directory": scratch_project.canonical_path(),
                     "stream_function": "stream:main"},
        "node_count": null, "failed_because": null,
    }]})
    .to_string();
    let restarted_runtime = isolated_machine_directories.serve_stub_local_api(StubLocalApiScript {
        tool_answers_by_name: HashMap::from([(
            "list_streams".to_owned(),
            vec![
                StubToolAnswer::tool_result(&not_yet_re_loaded),
                StubToolAnswer::tool_result(&not_yet_re_loaded),
                the_stream_listed_running_from(&scratch_project.canonical_path()),
            ],
        )]),
        stream_log_records: Some(StubStreamLogRecords {
            records: vec![a_log_record(json!({"message": "after the re-load"}))],
            page_size: 1,
        }),
        ..StubLocalApiScript::default()
    });
    running_tatolab.wait_for_standard_error_line_containing(
        "tatolab dev: the runtime answers again — waiting for it to re-load cam",
    );

    assert_eq!(
        running_tatolab.next_standard_output_line("the re-loaded stream's first record"),
        rendered_line("after the re-load")
    );
    assert!(calls_to(&restarted_runtime, "list_streams") >= 3);
    assert_eq!(calls_to(&restarted_runtime, "run_stream"), 0);
    let (exit_status, standard_error_lines) =
        signal_and_wait_for_a_prompt_exit(running_tatolab, libc::SIGINT);
    assert_eq!(exit_status.code(), Some(0), "{standard_error_lines:?}");
}

/// A restarted runtime that recorded the stream failed leaves `dev` waiting for a save.
#[test]
fn dev_waits_for_a_save_when_the_restarted_runtime_holds_the_stream_failed() {
    let isolated_machine_directories = IsolatedMachineDirectories::new();
    let scratch_project = ScratchProject::new();
    let first_runtime = a_runtime_holding(
        &isolated_machine_directories,
        &scratch_project.canonical_path(),
        vec![loaded_answer(&scratch_project.canonical_path())],
        vec![a_log_record(json!({"message": "before the crash"}))],
    );
    let mut running_tatolab = spawn_dev(&isolated_machine_directories, &scratch_project);
    running_tatolab.next_standard_output_line("the record before the crash");

    drop(first_runtime);
    running_tatolab.wait_for_standard_error_line_containing("waiting for a runtime to answer");
    let restarted_runtime = isolated_machine_directories.serve_stub_local_api(StubLocalApiScript {
        tool_answers_by_name: HashMap::from([(
            "list_streams".to_owned(),
            vec![StubToolAnswer::tool_result(
                &json!({"streams": [{
                    "stream": STREAM_ID, "display_name": "main", "state": "failed",
                    "location": null, "node_count": null,
                    "failed_because": "it was implicated in the runtime's last 2 crashes",
                }]})
                .to_string(),
            )],
        )]),
        ..StubLocalApiScript::default()
    });

    running_tatolab.wait_for_standard_error_line_containing(
        "tatolab dev: cam came back failed, so it is not followed; save again to load it",
    );
    assert!(running_tatolab.is_still_running());
    assert_eq!(calls_to(&restarted_runtime, "run_stream"), 0);
    let (exit_status, standard_error_lines) =
        signal_and_wait_for_a_prompt_exit(running_tatolab, libc::SIGINT);
    assert_eq!(exit_status.code(), Some(0), "{standard_error_lines:?}");
}

#[test]
fn ctrl_c_while_dev_reconnects_to_a_runtime_that_never_opens_the_connection_exits_zero() {
    let isolated_machine_directories = IsolatedMachineDirectories::new();
    let scratch_project = ScratchProject::new();
    let first_runtime = a_runtime_holding(
        &isolated_machine_directories,
        &scratch_project.canonical_path(),
        vec![loaded_answer(&scratch_project.canonical_path())],
        Vec::new(),
    );
    let mut running_tatolab = spawn_dev(&isolated_machine_directories, &scratch_project);
    running_tatolab
        .wait_for_standard_error_line_containing(&running_note(&scratch_project.canonical_path()));

    drop(first_runtime);
    running_tatolab.wait_for_standard_error_line_containing("waiting for a runtime to answer");
    let unanswering_runtime =
        isolated_machine_directories.serve_stub_local_api(StubLocalApiScript {
            mcp_stdio_upgrade_answer: StubMcpStdioUpgradeAnswer::NeverAnswer,
            ..StubLocalApiScript::default()
        });
    wait_until("the reconnect's /mcp/stdio upgrade request", || {
        !unanswering_runtime
            .recorded_mcp_stdio_request_heads()
            .is_empty()
    });

    let (exit_status, standard_error_lines) =
        signal_and_wait_for_a_prompt_exit(running_tatolab, libc::SIGINT);

    assert_eq!(exit_status.code(), Some(0), "{standard_error_lines:?}");
}

/// A machine whose runtime lists `cam` from `project_directory`, describes its log folder, and
/// pages `records`.
fn a_runtime_logs_follows(
    isolated_machine_directories: &IsolatedMachineDirectories,
    project_directory: &Path,
    records: Vec<serde_json::Value>,
) -> StubLocalApiServer {
    isolated_machine_directories.serve_stub_local_api(StubLocalApiScript {
        tool_answers_by_name: HashMap::from([
            (
                "list_streams".to_owned(),
                vec![the_stream_listed_running_from(project_directory)],
            ),
            (
                "describe_runtime".to_owned(),
                vec![a_runtime_logging_in("/home/u/.local/state/tatolab/logs")],
            ),
        ]),
        stream_log_records: Some(StubStreamLogRecords {
            records,
            page_size: 1,
        }),
        ..StubLocalApiScript::default()
    })
}

/// Ctrl-C ends `logs -f` with exit 0 and leaves the stream loaded.
#[test]
fn ctrl_c_on_logs_follow_exits_zero_and_never_stops_the_stream() {
    let isolated_machine_directories = IsolatedMachineDirectories::new();
    let scratch_project = ScratchProject::new();
    let stub_local_api_server = a_runtime_logs_follows(
        &isolated_machine_directories,
        &scratch_project.canonical_path(),
        vec![a_log_record(json!({"message": "running"}))],
    );
    let running_tatolab = RunningTatolab::spawn(
        isolated_machine_directories.tatolab_command(&["logs", "-f", "--stream", STREAM_ID]),
    );
    assert_eq!(
        running_tatolab.next_standard_output_line("the stream's record"),
        rendered_line("running")
    );

    let (exit_status, standard_error_lines) =
        signal_and_wait_for_a_prompt_exit(running_tatolab, libc::SIGINT);

    assert_eq!(exit_status.code(), Some(0), "{standard_error_lines:?}");
    assert_eq!(calls_to(&stub_local_api_server, "stop_stream"), 0);
    assert_eq!(
        tool_calls_besides_logs(&stub_local_api_server),
        ["list_streams"]
    );
}

/// A runtime that goes away mid-follow ends `logs -f` with exit 1, naming the crash and the log
/// folder the runtime described.
#[test]
fn a_runtime_gone_mid_follow_ends_logs_follow_with_one_naming_its_log() {
    let isolated_machine_directories = IsolatedMachineDirectories::new();
    let scratch_project = ScratchProject::new();
    let stub_local_api_server = a_runtime_logs_follows(
        &isolated_machine_directories,
        &scratch_project.canonical_path(),
        vec![a_log_record(json!({"message": "running"}))],
    );
    let running_tatolab = RunningTatolab::spawn(
        isolated_machine_directories.tatolab_command(&["logs", "-f", "--stream", STREAM_ID]),
    );
    running_tatolab.next_standard_output_line("the stream's record");

    drop(stub_local_api_server);
    let (exit_status, standard_error_lines) = running_tatolab.wait_for_exit();

    assert_eq!(exit_status.code(), Some(1), "{standard_error_lines:?}");
    assert_eq!(
        standard_error_lines.last().unwrap(),
        "error: the runtime went away — it crashed or was stopped; its log is under \
         /home/u/.local/state/tatolab/logs/"
    );
}

/// `logs -f` follows a stream loaded again — a `dev` save elsewhere — from its first record.
#[test]
fn logs_follow_reads_a_stream_loaded_again_mid_follow_from_its_first_record() {
    let isolated_machine_directories = IsolatedMachineDirectories::new();
    let scratch_project = ScratchProject::new();
    let stub_local_api_server = a_runtime_logs_follows(
        &isolated_machine_directories,
        &scratch_project.canonical_path(),
        vec![
            a_log_record(json!({"message": "first"})),
            a_log_record(json!({"message": "second"})),
        ],
    );
    let mut running_tatolab = RunningTatolab::spawn(
        isolated_machine_directories.tatolab_command(&["logs", "-f", "--stream", STREAM_ID]),
    );
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
