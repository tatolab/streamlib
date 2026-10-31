// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `tatolab logs` run as a user runs it: its flags, the project log it reads through a stub local
//! API served at an isolated machine's fixed socket, what it prints on stdout and on stderr, how
//! Ctrl-C ends a follow, and how it exits. A build without the test feature can isolate the
//! machine on Linux alone.

mod common;

use std::io::{BufRead, BufReader, Read};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use common::tatolab_binary_run::{
    run_tatolab_reading_no_runtime_directory, standard_error_text, standard_output_text,
};

/// How long a follow assertion waits before failing rather than hanging.
const FOLLOW_LINE_TIMEOUT: Duration = Duration::from_secs(15);

/// A `tatolab logs --follow` running in the background, its output read line by line.
struct FollowingTatolab {
    tatolab_process: Child,
    standard_output_lines: Receiver<String>,
}

impl FollowingTatolab {
    fn spawn(mut tatolab_command: Command) -> Self {
        let mut tatolab_process = tatolab_command
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        Self {
            standard_output_lines: lines_read_in_the_background(
                tatolab_process.stdout.take().unwrap(),
            ),
            tatolab_process,
        }
    }

    fn next_standard_output_line(&self, awaited: &str) -> String {
        self.standard_output_lines
            .recv_timeout(FOLLOW_LINE_TIMEOUT)
            .unwrap_or_else(|_| {
                panic!("expected {awaited} on stdout within {FOLLOW_LINE_TIMEOUT:?}")
            })
    }

    /// Ctrl-C, as a terminal delivers it, and the exit that follows.
    fn interrupt_and_wait(mut self) -> ExitStatus {
        // SAFETY: `kill` takes a pid and a signal number and touches no memory.
        let kill_result =
            unsafe { libc::kill(self.tatolab_process.id() as libc::pid_t, libc::SIGINT) };
        assert_eq!(kill_result, 0);
        let exit_deadline = Instant::now() + FOLLOW_LINE_TIMEOUT;
        loop {
            if let Some(exit_status) = self.tatolab_process.try_wait().unwrap() {
                return exit_status;
            }
            if Instant::now() > exit_deadline {
                self.tatolab_process.kill().unwrap();
                panic!(
                    "`tatolab logs --follow` did not end within {FOLLOW_LINE_TIMEOUT:?} of Ctrl-C"
                );
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

fn lines_read_in_the_background(stream: impl Read + Send + 'static) -> Receiver<String> {
    let (line_sender, line_receiver) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stream).lines() {
            let Ok(line) = line else { return };
            if line_sender.send(line).is_err() {
                return;
            }
        }
    });
    line_receiver
}

fn rendered_line(message: &str, level_column: &str) -> String {
    format!("21:04:27.573 [{level_column}] [Rabc/rust] tatolabd — {message}")
}

#[test]
fn the_logs_help_names_the_project_and_every_flag() {
    let help_text = standard_output_text(&run_tatolab_reading_no_runtime_directory(&[
        "logs", "--help",
    ]));

    for named in [
        "--dir <DIR>",
        "default: CWD",
        "--stream <STREAM>",
        "every record of the project",
        "--action <ID>",
        "-f, --follow",
        "like `tail -F`",
        "--processor <ID>",
        "--pipeline <ID>",
        "--rhi",
        "--level <LEVEL>",
        "[possible values: trace, debug, info, warn, error]",
        "--source <SOURCE>",
        "[possible values: rust, python]",
        "--intercepted-only",
    ] {
        assert!(help_text.contains(named), "{named}:\n{help_text}");
    }
    for removed in ["RUNTIME_ID-STREAM", "--list"] {
        assert!(!help_text.contains(removed), "{removed}:\n{help_text}");
    }
    assert!(!help_text.contains("StreamLib"), "{help_text}");
}

/// Control is reachable only through a runtime's local API socket, so `logs` dials no address.
#[test]
fn logs_takes_no_network_address_for_the_control_plane() {
    let refused =
        run_tatolab_reading_no_runtime_directory(&["logs", "--url", "http://127.0.0.1:9100"]);

    assert_eq!(refused.status.code(), Some(2));
    assert!(
        standard_error_text(&refused).contains("unexpected argument '--url'"),
        "{}",
        standard_error_text(&refused)
    );
}

#[test]
fn an_unknown_level_is_a_usage_error_naming_the_levels() {
    let refused = run_tatolab_reading_no_runtime_directory(&["logs", "--level", "fatal"]);

    assert_eq!(refused.status.code(), Some(2));
    assert!(
        standard_error_text(&refused).contains("trace, debug, info, warn, error"),
        "{}",
        standard_error_text(&refused)
    );
}

#[test]
fn a_file_name_or_list_is_a_usage_error() {
    for removed in [&["Rabc-camera"][..], &["--list"]] {
        let refused = run_tatolab_reading_no_runtime_directory(&[&["logs"][..], removed].concat());

        assert_eq!(refused.status.code(), Some(2), "{removed:?}");
    }
}

#[cfg(any(target_os = "linux", feature = "machine-directories-under-a-test-root"))]
mod against_an_isolated_machine {
    use serde_json::json;

    use super::common::isolated_machine_directories::IsolatedMachineDirectories;
    use super::common::runtime_log_line_fixtures::a_log_record;
    use super::common::stub_local_api_server::{
        StubLocalApiScript, StubStreamLogRecords, StubToolAnswer,
    };
    use super::common::tatolab_binary_run::{standard_error_text, standard_output_text};
    use super::{FollowingTatolab, rendered_line};

    /// The id of the stream the stub runtime lists as `camera`.
    const DESK_CAMERA_STREAM_ID: &str = "desk-camera-0a1b2c";

    fn records_saying(messages: &[&str]) -> Vec<serde_json::Value> {
        messages
            .iter()
            .map(|message| a_log_record(json!({"message": message})))
            .collect()
    }

    /// A runtime listing `camera` from `project_directory`, whose `logs` pages `records` of the
    /// project log two at a time.
    fn a_runtime_holding_the_desk_camera_paging(
        records: Vec<serde_json::Value>,
        project_directory: &std::path::Path,
    ) -> StubLocalApiScript {
        StubLocalApiScript {
            stream_log_records: Some(StubStreamLogRecords {
                records,
                page_size: 2,
                first_written_by_a_run: false,
            }),
            tool_answers_by_name: std::collections::HashMap::from([(
                "list_streams".to_owned(),
                vec![StubToolAnswer::tool_result(
                    &json!({"streams": [{
                        "stream": DESK_CAMERA_STREAM_ID, "display_name": "camera", "state": "running",
                        "location": {"project_directory": project_directory,
                                     "stream_function": "stream:camera"},
                        "node_count": 3, "failed_because": null,
                    }]})
                    .to_string(),
                )],
            )]),
            ..StubLocalApiScript::default()
        }
    }

    #[test]
    fn logs_reads_the_working_directorys_project_page_by_page_to_the_newest() {
        let project_directory = tempfile::tempdir().unwrap();
        let canonical_project_directory = project_directory.path().canonicalize().unwrap();
        let isolated_machine_directories = IsolatedMachineDirectories::new();
        let stub_local_api_server = isolated_machine_directories.serve_stub_local_api(
            a_runtime_holding_the_desk_camera_paging(
                records_saying(&["started", "running", "slowing"]),
                &canonical_project_directory,
            ),
        );

        let read = isolated_machine_directories
            .tatolab_command(&["logs"])
            .current_dir(project_directory.path())
            .output()
            .unwrap();

        assert!(read.status.success(), "{}", standard_error_text(&read));
        assert_eq!(
            standard_output_text(&read),
            format!(
                "{}\n{}\n{}\n",
                rendered_line("started", " INFO"),
                rendered_line("running", " INFO"),
                rendered_line("slowing", " INFO")
            )
        );
        assert_eq!(standard_error_text(&read), "");
        assert_eq!(
            stub_local_api_server.recorded_arguments_of("logs"),
            [
                json!({"project_directory": canonical_project_directory}),
                json!({"project_directory": canonical_project_directory, "after": "1:2"}),
                json!({"project_directory": canonical_project_directory, "after": "1:3"}),
            ]
        );
        assert_eq!(
            stub_local_api_server.recorded_arguments_of("list_streams"),
            Vec::<serde_json::Value>::new(),
            "every record of the project names no stream to resolve"
        );
    }

    #[test]
    fn logs_of_a_stream_by_display_name_asks_the_project_for_its_id_and_the_action() {
        let project_directory = tempfile::tempdir().unwrap();
        let canonical_project_directory = project_directory.path().canonicalize().unwrap();
        let isolated_machine_directories = IsolatedMachineDirectories::new();
        let stub_local_api_server = isolated_machine_directories.serve_stub_local_api(
            a_runtime_holding_the_desk_camera_paging(
                records_saying(&["started"]),
                &canonical_project_directory,
            ),
        );

        let read = isolated_machine_directories.run_tatolab(&[
            "logs",
            "--dir",
            project_directory.path().to_str().unwrap(),
            "--stream",
            "camera",
            "--action",
            "a7",
        ]);

        assert!(read.status.success(), "{}", standard_error_text(&read));
        assert_eq!(
            stub_local_api_server.recorded_arguments_of("logs")[0],
            json!({
                "project_directory": canonical_project_directory,
                "stream": DESK_CAMERA_STREAM_ID,
                "action": "a7",
            }),
            "the display name resolves to the id, which is all the tool sees"
        );
    }

    /// A stream removed, or recorded by no runtime since a restart, still has its records in the
    /// project's log, read by the id it had.
    #[test]
    fn logs_of_a_stream_the_runtime_no_longer_lists_reads_it_by_its_id() {
        let isolated_machine_directories = IsolatedMachineDirectories::new();
        let stub_local_api_server = isolated_machine_directories.serve_stub_local_api(
            a_runtime_holding_the_desk_camera_paging(
                records_saying(&["before the rm"]),
                std::path::Path::new("/srv/desk"),
            ),
        );

        let read =
            isolated_machine_directories.run_tatolab(&["logs", "--stream", "desk-gone-9a8b"]);

        assert!(read.status.success(), "{}", standard_error_text(&read));
        assert_eq!(
            standard_output_text(&read),
            format!("{}\n", rendered_line("before the rm", " INFO"))
        );
        assert_eq!(
            stub_local_api_server.recorded_arguments_of("logs")[0]["stream"],
            "desk-gone-9a8b"
        );
    }

    #[test]
    fn logs_of_a_directory_that_is_not_one_is_refused_before_asking_the_runtime() {
        let isolated_machine_directories = IsolatedMachineDirectories::new();

        let refused =
            isolated_machine_directories.run_tatolab(&["logs", "--dir", "/nonexistent-project"]);

        assert_eq!(refused.status.code(), Some(1));
        assert_eq!(
            standard_error_text(&refused),
            "error: --dir /nonexistent-project is not a directory\n"
        );
    }

    /// Ctrl-C out of `--follow` is how it ends, not a failure: exit 0, not 130.
    #[test]
    fn logs_followed_reads_on_across_pages_by_cursor_until_ctrl_c() {
        let isolated_machine_directories = IsolatedMachineDirectories::new();
        let stub_local_api_server = isolated_machine_directories.serve_stub_local_api(
            a_runtime_holding_the_desk_camera_paging(
                records_saying(&["first", "second", "third"]),
                std::path::Path::new("/srv/desk"),
            ),
        );
        let following_tatolab = FollowingTatolab::spawn(
            isolated_machine_directories.tatolab_command(&["logs", "--stream", "camera", "-f"]),
        );
        for message in ["first", "second", "third"] {
            assert_eq!(
                following_tatolab.next_standard_output_line(message),
                rendered_line(message, " INFO")
            );
        }

        stub_local_api_server.append_stream_log_records(records_saying(&["fourth", "fifth"]));
        for message in ["fourth", "fifth"] {
            assert_eq!(
                following_tatolab.next_standard_output_line(message),
                rendered_line(message, " INFO")
            );
        }

        assert_eq!(following_tatolab.interrupt_and_wait().code(), Some(0));
        let afters_asked_for: Vec<serde_json::Value> = stub_local_api_server
            .recorded_arguments_of("logs")
            .iter()
            .map(|logs_arguments| logs_arguments.get("after").cloned().unwrap_or_default())
            .collect();
        assert_eq!(afters_asked_for[..2], [json!(null), json!("1:2")]);
        assert!(afters_asked_for.contains(&json!("1:3")));
        assert!(afters_asked_for.contains(&json!("1:5")));
    }
}
