// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `tatolab dev` and `tatolab logs -f` run as a user runs them, against a stub local API at an
//! isolated machine's fixed socket: `dev` queues a load of the project over a `/mcp/stdio`
//! connection of its own, follows the project's log and queues another load for each saved edit;
//! Ctrl-C ends either verb and never stops a stream; a runtime that goes away ends `logs -f`
//! naming its log, while `dev` waits for it and follows the log again without queuing a load.

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
    StubToolAnswer, StubToolCallTransport, run_project_tool_result_text,
};

/// The stream `logs -f` follows here.
const STREAM_ID: &str = "cam";

/// Far inside the 30 s `dev`'s connection waits on an unanswered request: a stop signal that
/// waited on one would not exit within it.
const STOP_SIGNAL_EXIT_BOUND: Duration = Duration::from_secs(5);

/// What `dev` notes when a stop signal ends its following.
const STOPPED_FOLLOWING_NOTE: &str = "tatolab dev: stopped following; the project's streams keep \
    running — `tatolab stop` stops one, `tatolab rm` removes the project";

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

/// `run_project`'s answer: the load `action` of the project in `project_directory` queued.
fn queued_answer(action: &str, project_directory: &Path) -> StubToolAnswer {
    StubToolAnswer::tool_result(&run_project_tool_result_text(action, project_directory))
}

/// `dev`'s note once the load `action` of `project_directory` is queued.
fn queued_note(action: &str, project_directory: &Path) -> String {
    format!(
        "tatolab dev: {action} queued: load of {}",
        project_directory.display()
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

/// A runtime that answers each `run_project` as `run_project_answers` say, in order, and pages the
/// project log one record per `logs` call, `project_log_records` written by the first load.
fn a_runtime_holding(
    isolated_machine_directories: &IsolatedMachineDirectories,
    run_project_answers: Vec<StubToolAnswer>,
    project_log_records: Vec<serde_json::Value>,
) -> StubLocalApiServer {
    isolated_machine_directories.serve_stub_local_api(StubLocalApiScript {
        tool_answers_by_name: HashMap::from([("run_project".to_owned(), run_project_answers)]),
        stream_log_records: Some(StubStreamLogRecords {
            records: project_log_records,
            page_size: 1,
            first_written_by_a_run: true,
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

/// A record as `dev` prints it: labelled by its project's folder name.
fn dev_rendered_line(scratch_project: &ScratchProject, message: &str) -> String {
    let projects_folder_name = scratch_project.canonical_path();
    format!(
        "{} {}",
        projects_folder_name.file_name().unwrap().to_string_lossy(),
        rendered_line(message)
    )
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
fn dev_queues_a_load_of_the_project_and_prints_each_record_under_its_folder_name() {
    let isolated_machine_directories = IsolatedMachineDirectories::new();
    let scratch_project = ScratchProject::new();
    let stub_local_api_server = a_runtime_holding(
        &isolated_machine_directories,
        vec![queued_answer("Rabc-1", &scratch_project.canonical_path())],
        vec![
            a_log_record(json!({"message": "first"})),
            a_log_record(json!({"message": "second"})),
        ],
    );

    let mut running_tatolab = spawn_dev(&isolated_machine_directories, &scratch_project);

    running_tatolab.wait_for_standard_error_line_containing(&queued_note(
        "Rabc-1",
        &scratch_project.canonical_path(),
    ));
    assert_eq!(
        running_tatolab.next_standard_output_line("the first record"),
        dev_rendered_line(&scratch_project, "first")
    );
    assert_eq!(
        running_tatolab.next_standard_output_line("the second record"),
        dev_rendered_line(&scratch_project, "second")
    );
    wait_until("a fourth `logs` call", || {
        calls_to(&stub_local_api_server, "logs") >= 4
    });
    assert_eq!(
        stub_local_api_server.recorded_arguments_of("run_project"),
        [json!({"project_directory": scratch_project.canonical_path()})]
    );
    assert_eq!(
        stub_local_api_server
            .recorded_mcp_stdio_request_heads()
            .len(),
        1,
        "dev rides one /mcp/stdio connection of its own"
    );
    assert_every_call_rode_the_mcp_stdio_connection(&stub_local_api_server, "run_project");
    assert_every_call_rode_the_mcp_stdio_connection(&stub_local_api_server, "logs");
    let logs_afters: Vec<serde_json::Value> = stub_local_api_server
        .recorded_arguments_of("logs")
        .iter()
        .map(|logs_arguments| {
            assert_eq!(
                logs_arguments["project_directory"],
                json!(scratch_project.canonical_path())
            );
            assert!(logs_arguments.get("stream").is_none(), "{logs_arguments}");
            assert!(logs_arguments.get("action").is_none(), "{logs_arguments}");
            logs_arguments.get("after").cloned().unwrap_or_default()
        })
        .collect();
    assert_eq!(
        logs_afters[..4],
        [json!(null), json!("1:0"), json!("1:1"), json!("1:2")],
        "the log is read to its end before the load, then each record after the last one read"
    );
    drop(running_tatolab);
}

/// Ctrl-C ends `dev`'s following and leaves the project's streams loaded: no `stop_stream`, no
/// `remove_project`, exit 0.
#[test]
fn ctrl_c_on_dev_ends_the_following_and_never_stops_a_stream() {
    for stop_signal in [libc::SIGINT, libc::SIGTERM] {
        let isolated_machine_directories = IsolatedMachineDirectories::new();
        let scratch_project = ScratchProject::new();
        let stub_local_api_server = a_runtime_holding(
            &isolated_machine_directories,
            vec![queued_answer("Rabc-1", &scratch_project.canonical_path())],
            vec![a_log_record(json!({"message": "running"}))],
        );
        let running_tatolab = spawn_dev(&isolated_machine_directories, &scratch_project);
        running_tatolab.next_standard_output_line("the stream's record");

        let (exit_status, standard_error_lines) =
            signal_and_wait_for_a_prompt_exit(running_tatolab, stop_signal);

        assert_eq!(exit_status.code(), Some(0), "{standard_error_lines:?}");
        assert_eq!(
            tool_calls_besides_logs(&stub_local_api_server),
            ["run_project"]
        );
        assert_eq!(standard_error_lines.last().unwrap(), STOPPED_FOLLOWING_NOTE);
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
        ["tatolab dev: stopped before the runtime opened a connection; nothing was queued"]
    );
    assert!(stub_local_api_server.recorded_tool_calls().is_empty());
}

/// A refused load ends nothing: `dev` names the refusal, follows the project's log on, and each
/// save queues another load over the same connection, never stopping a stream first.
#[test]
fn dev_follows_on_after_a_refused_load_and_queues_another_load_for_each_save() {
    let isolated_machine_directories = IsolatedMachineDirectories::new();
    let scratch_project = ScratchProject::new();
    let refusal = "the runtime holds no state directory, so it records no stream";
    let stub_local_api_server = a_runtime_holding(
        &isolated_machine_directories,
        vec![
            StubToolAnswer::tool_failure(refusal),
            queued_answer("Rabc-2", &scratch_project.canonical_path()),
            queued_answer("Rabc-3", &scratch_project.canonical_path()),
        ],
        vec![a_log_record(
            json!({"message": "written by the refused load"}),
        )],
    );
    let mut running_tatolab = spawn_dev(&isolated_machine_directories, &scratch_project);
    running_tatolab.wait_for_standard_error_line_containing(&format!(
        "tatolab dev: run_project failed: {refusal}; save again to queue another load"
    ));
    assert_eq!(
        running_tatolab.next_standard_output_line("the record the refused load wrote"),
        dev_rendered_line(&scratch_project, "written by the refused load")
    );

    scratch_project.save_stream_py("# the second version, longer than the first\n");
    running_tatolab.wait_for_standard_error_line_containing(&queued_note(
        "Rabc-2",
        &scratch_project.canonical_path(),
    ));
    stub_local_api_server.append_stream_log_records(vec![a_log_record(
        json!({"message": "the stream `cam` failed: ValueError: bad wiring"}),
    )]);
    assert_eq!(
        running_tatolab.next_standard_output_line("the failed load's record"),
        dev_rendered_line(
            &scratch_project,
            "the stream `cam` failed: ValueError: bad wiring"
        ),
        "a save whose code fails is a record of the project's log, followed on"
    );
    assert!(
        running_tatolab.is_still_running(),
        "a failed load ends nothing"
    );

    scratch_project.save_stream_py("# the fixed version, longer than every save before it\n");
    running_tatolab.wait_for_standard_error_line_containing(&queued_note(
        "Rabc-3",
        &scratch_project.canonical_path(),
    ));
    let (exit_status, standard_error_lines) =
        signal_and_wait_for_a_prompt_exit(running_tatolab, libc::SIGINT);

    assert_eq!(exit_status.code(), Some(0), "{standard_error_lines:?}");
    assert_eq!(
        tool_calls_besides_logs(&stub_local_api_server),
        ["run_project", "run_project", "run_project"],
        "each save queues a load over the connection; nothing stops a stream"
    );
    assert_eq!(
        stub_local_api_server
            .recorded_mcp_stdio_request_heads()
            .len(),
        1,
        "every load rides the one connection"
    );
    assert_every_call_rode_the_mcp_stdio_connection(&stub_local_api_server, "run_project");
}

/// A page `dev` cannot read leaves the streams running unfollowed, never replaying the project's
/// whole log, and waits for a save rather than driving a load loop.
#[test]
fn dev_waits_for_a_save_when_the_runtime_answers_logs_with_a_page_it_cannot_read() {
    let isolated_machine_directories = IsolatedMachineDirectories::new();
    let scratch_project = ScratchProject::new();
    let stub_local_api_server =
        isolated_machine_directories.serve_stub_local_api(StubLocalApiScript {
            tool_answers_by_name: HashMap::from([
                (
                    "run_project".to_owned(),
                    vec![queued_answer("Rabc-1", &scratch_project.canonical_path())],
                ),
                (
                    "logs".to_owned(),
                    vec![StubToolAnswer::tool_result(r#"{"stream": "cam"}"#)],
                ),
            ]),
            ..StubLocalApiScript::default()
        });
    let mut running_tatolab = spawn_dev(&isolated_machine_directories, &scratch_project);
    running_tatolab
        .wait_for_standard_error_line_containing("save again to queue another load and follow it");
    // Ten of `dev`'s runtime polls: a page it cannot read must not drive a load loop.
    std::thread::sleep(Duration::from_millis(2500));

    assert!(running_tatolab.is_still_running());
    assert_eq!(
        tool_calls_besides_logs(&stub_local_api_server),
        ["run_project"]
    );
    assert_eq!(
        calls_to(&stub_local_api_server, "logs"),
        2,
        "the read to the log's end before the load, once more before the follow, and no follow \
         from an unknown end"
    );

    scratch_project.save_stream_py("# the edited version, longer than the first\n");
    wait_until("the load after the save", || {
        calls_to(&stub_local_api_server, "run_project") == 2
    });
    let (exit_status, standard_error_lines) =
        signal_and_wait_for_a_prompt_exit(running_tatolab, libc::SIGINT);
    assert_eq!(exit_status.code(), Some(0), "{standard_error_lines:?}");
    assert_eq!(calls_to(&stub_local_api_server, "stop_stream"), 0);
}

/// The runtime closing `dev`'s connection is a crash: `dev` names it and the log folder the
/// runtime described, waits for the runtime, and follows the project's log on from where it read
/// up to while the runtime re-loads the project from its records — never queuing a load for it.
#[test]
fn dev_survives_a_dropped_runtime_and_follows_the_log_on_without_queuing_a_load() {
    let isolated_machine_directories = IsolatedMachineDirectories::new();
    let scratch_project = ScratchProject::new();
    let stub_local_api_server =
        isolated_machine_directories.serve_stub_local_api(StubLocalApiScript {
            tool_answers_by_name: HashMap::from([
                (
                    "run_project".to_owned(),
                    vec![queued_answer("Rabc-1", &scratch_project.canonical_path())],
                ),
                (
                    "describe_runtime".to_owned(),
                    vec![a_runtime_logging_in("/home/u/.local/state/tatolab/logs")],
                ),
            ]),
            stream_log_records: Some(StubStreamLogRecords {
                records: vec![a_log_record(json!({"message": "before the crash"}))],
                page_size: 1,
                first_written_by_a_run: true,
            }),
            ..StubLocalApiScript::default()
        });
    let mut running_tatolab = spawn_dev(&isolated_machine_directories, &scratch_project);
    assert_eq!(
        running_tatolab.next_standard_output_line("the record before the crash"),
        dev_rendered_line(&scratch_project, "before the crash")
    );

    stub_local_api_server
        .append_stream_log_records(vec![a_log_record(json!({"message": "after the re-load"}))]);
    stub_local_api_server.close_every_mcp_stdio_connection();
    running_tatolab.wait_for_standard_error_line_containing(
        "tatolab dev: the runtime went away — it crashed or was stopped; its log is under \
         /home/u/.local/state/tatolab/logs/",
    );
    running_tatolab.wait_for_standard_error_line_containing(
        "tatolab dev: the runtime answers again — following the project's log on",
    );
    assert_eq!(
        running_tatolab.next_standard_output_line("the re-loaded stream's first record"),
        dev_rendered_line(&scratch_project, "after the re-load")
    );
    let (exit_status, standard_error_lines) =
        signal_and_wait_for_a_prompt_exit(running_tatolab, libc::SIGINT);

    assert_eq!(exit_status.code(), Some(0), "{standard_error_lines:?}");
    assert_eq!(
        calls_to(&stub_local_api_server, "run_project"),
        1,
        "the crash never queues another load"
    );
    assert_eq!(
        stub_local_api_server
            .recorded_mcp_stdio_request_heads()
            .len(),
        2,
        "the follow after the crash rides a new connection"
    );
}

/// A load the runtime went away before answering stays owed: once a runtime answers again, `dev`
/// queues it there rather than following a log no load of its own writes to.
#[test]
fn a_load_the_runtime_went_away_before_answering_is_queued_again_once_it_answers() {
    let isolated_machine_directories = IsolatedMachineDirectories::new();
    let scratch_project = ScratchProject::new();
    let stub_local_api_server =
        isolated_machine_directories.serve_stub_local_api(StubLocalApiScript {
            tools_that_never_answer: ["run_project".to_owned()].into_iter().collect(),
            stream_log_records: Some(StubStreamLogRecords {
                records: Vec::new(),
                page_size: 1,
                first_written_by_a_run: true,
            }),
            ..StubLocalApiScript::default()
        });
    let running_tatolab = spawn_dev(&isolated_machine_directories, &scratch_project);
    wait_until("dev's first load reaches the runtime", || {
        calls_to(&stub_local_api_server, "run_project") == 1
    });

    stub_local_api_server.close_every_mcp_stdio_connection();
    wait_until("dev queues the load again once the runtime answers", || {
        calls_to(&stub_local_api_server, "run_project") == 2
    });
    let (exit_status, standard_error_lines) =
        signal_and_wait_for_a_prompt_exit(running_tatolab, libc::SIGINT);

    assert_eq!(exit_status.code(), Some(0), "{standard_error_lines:?}");
    assert!(
        standard_error_lines
            .iter()
            .any(|line| line.starts_with("tatolab dev: the runtime went away")),
        "{standard_error_lines:?}"
    );
    assert_eq!(
        stub_local_api_server
            .recorded_mcp_stdio_request_heads()
            .len(),
        2,
        "the load is asked again over a new connection"
    );
}

/// A restarted runtime is followed from the cursor the session read up to: the project's log is
/// on disk, so a record written before the crash is never printed twice.
#[test]
fn dev_follows_a_restarted_runtimes_project_log_from_where_it_read_up_to() {
    let isolated_machine_directories = IsolatedMachineDirectories::new();
    let scratch_project = ScratchProject::new();
    let first_runtime = a_runtime_holding(
        &isolated_machine_directories,
        vec![queued_answer("Rabc-1", &scratch_project.canonical_path())],
        vec![a_log_record(json!({"message": "before the crash"}))],
    );
    let mut running_tatolab = spawn_dev(&isolated_machine_directories, &scratch_project);
    running_tatolab.next_standard_output_line("the record before the crash");

    drop(first_runtime);
    running_tatolab.wait_for_standard_error_line_containing("tatolab dev: the runtime went away");
    running_tatolab.wait_for_standard_error_line_containing("waiting for a runtime to answer");
    let restarted_runtime = isolated_machine_directories.serve_stub_local_api(StubLocalApiScript {
        stream_log_records: Some(StubStreamLogRecords {
            records: vec![
                a_log_record(json!({"message": "before the crash"})),
                a_log_record(json!({"message": "after the re-load"})),
            ],
            page_size: 1,
            first_written_by_a_run: false,
        }),
        ..StubLocalApiScript::default()
    });

    assert_eq!(
        running_tatolab.next_standard_output_line("the re-loaded stream's first record"),
        dev_rendered_line(&scratch_project, "after the re-load")
    );
    assert_eq!(
        tool_calls_besides_logs(&restarted_runtime),
        Vec::<String>::new()
    );
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
        vec![queued_answer("Rabc-1", &scratch_project.canonical_path())],
        Vec::new(),
    );
    let mut running_tatolab = spawn_dev(&isolated_machine_directories, &scratch_project);
    running_tatolab.wait_for_standard_error_line_containing(&queued_note(
        "Rabc-1",
        &scratch_project.canonical_path(),
    ));

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
            first_written_by_a_run: false,
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

/// `logs -f` reads a project's log on across a re-load of its stream — a `dev` save elsewhere —
/// since the log is the project's, not the load's.
#[test]
fn logs_follow_reads_on_across_a_re_load_of_the_stream() {
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
    let running_tatolab = RunningTatolab::spawn(
        isolated_machine_directories.tatolab_command(&["logs", "-f", "--stream", STREAM_ID]),
    );
    for message in ["first", "second"] {
        assert_eq!(
            running_tatolab.next_standard_output_line(message),
            rendered_line(message)
        );
    }

    stub_local_api_server
        .append_stream_log_records(vec![a_log_record(json!({"message": "after the re-load"}))]);

    assert_eq!(
        running_tatolab.next_standard_output_line("the re-loaded stream's first record"),
        rendered_line("after the re-load")
    );
    let (exit_status, standard_error_lines) =
        signal_and_wait_for_a_prompt_exit(running_tatolab, libc::SIGINT);
    assert_eq!(exit_status.code(), Some(0), "{standard_error_lines:?}");
    assert!(
        stub_local_api_server
            .recorded_arguments_of("logs")
            .iter()
            .all(|logs_arguments| logs_arguments["stream"] == STREAM_ID),
        "every page asks for the one stream"
    );
}
