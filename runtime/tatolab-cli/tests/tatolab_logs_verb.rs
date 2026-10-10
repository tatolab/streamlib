// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `tatolab logs` run as a user runs it: its flags, the log directory it reads under
//! `STREAMLIB_HOME`, what it prints on stdout and on stderr, how Ctrl-C ends a follow, and how it
//! exits. Each reading scenario is the in-crate tests'. `--stream` reads a stub local API served
//! at an isolated machine's fixed socket, which a build without the test feature can isolate on
//! Linux alone.

mod common;

use std::io::{BufRead, BufReader, Read};
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Output, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use common::runtime_log_line_fixtures::{
    a_log_line_with_message, a_log_line_with_message_at_level, append_to_log_file,
};
use common::tatolab_binary_run::{
    run_tatolab_reading_no_runtime_directory, standard_error_text, standard_output_text,
};

/// How long a follow assertion waits before failing rather than hanging.
const FOLLOW_LINE_TIMEOUT: Duration = Duration::from_secs(15);

/// A `STREAMLIB_HOME` of a test's own, and the log directory a runtime writes under it.
struct IsolatedStreamlibHome {
    streamlib_home: tempfile::TempDir,
}

impl IsolatedStreamlibHome {
    fn new() -> Self {
        Self {
            streamlib_home: tempfile::tempdir().unwrap(),
        }
    }

    fn runtime_log_directory(&self) -> PathBuf {
        self.streamlib_home.path().join(".streamlib").join("logs")
    }

    fn write_log_file(&self, file_name: &str, log_lines: &str) -> PathBuf {
        let runtime_log_directory = self.runtime_log_directory();
        std::fs::create_dir_all(&runtime_log_directory).unwrap();
        let log_file_path = runtime_log_directory.join(file_name);
        std::fs::write(&log_file_path, log_lines).unwrap();
        log_file_path
    }

    fn tatolab_command(&self, tatolab_arguments: &[&str]) -> Command {
        let mut tatolab_command = Command::new(env!("CARGO_BIN_EXE_tatolab"));
        tatolab_command
            .args(tatolab_arguments)
            .env("STREAMLIB_HOME", self.streamlib_home.path())
            .stdin(Stdio::null());
        tatolab_command
    }

    fn run_tatolab(&self, tatolab_arguments: &[&str]) -> Output {
        self.tatolab_command(tatolab_arguments).output().unwrap()
    }
}

/// A `tatolab logs --follow` running in the background, its output read line by line.
struct FollowingTatolab {
    tatolab_process: Child,
    standard_output_lines: Receiver<String>,
    standard_error_lines: Receiver<String>,
}

impl FollowingTatolab {
    fn spawn(mut tatolab_command: Command) -> Self {
        let mut tatolab_process = tatolab_command
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        Self {
            standard_output_lines: lines_read_in_the_background(
                tatolab_process.stdout.take().unwrap(),
            ),
            standard_error_lines: lines_read_in_the_background(
                tatolab_process.stderr.take().unwrap(),
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

    fn next_standard_error_line(&self, awaited: &str) -> String {
        self.standard_error_lines
            .recv_timeout(FOLLOW_LINE_TIMEOUT)
            .unwrap_or_else(|_| {
                panic!("expected {awaited} on stderr within {FOLLOW_LINE_TIMEOUT:?}")
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
fn the_logs_help_names_both_modes_and_every_flag() {
    let help_text = standard_output_text(&run_tatolab_reading_no_runtime_directory(&[
        "logs", "--help",
    ]));

    for named in [
        "[RUNTIME_ID-STREAM]",
        "exactly as the runtime mirrored it",
        "as the runtime holds them",
        "Omit with --list or --stream",
        "--list",
        "--stream <STREAM>",
        "-f, --follow",
        "like `tail -F`",
        "by sequence number",
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
    assert!(!help_text.contains("StreamLib"), "{help_text}");
    assert!(!help_text.contains("node's"), "{help_text}");
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
    let refused = run_tatolab_reading_no_runtime_directory(&["logs", "Rabc", "--level", "fatal"]);

    assert_eq!(refused.status.code(), Some(2));
    assert!(
        standard_error_text(&refused).contains("trace, debug, info, warn, error"),
        "{}",
        standard_error_text(&refused)
    );
}

#[test]
fn a_runtime_with_no_log_file_is_refused_naming_list() {
    let isolated_streamlib_home = IsolatedStreamlibHome::new();

    let refused = isolated_streamlib_home.run_tatolab(&["logs", "Rnone"]);

    assert_eq!(refused.status.code(), Some(1));
    assert_eq!(
        standard_error_text(&refused),
        format!(
            "error: no stream log `Rnone` in {}.\nA runtime logs per loaded stream, as \
             `<runtime_id>-<stream>`; `tatolab logs --list` names each one.\n",
            isolated_streamlib_home.runtime_log_directory().display()
        )
    );
    assert_eq!(standard_output_text(&refused), "");
}

#[test]
fn list_prints_every_runtime_newest_started_first_summing_its_segments() {
    let isolated_streamlib_home = IsolatedStreamlibHome::new();
    isolated_streamlib_home.write_log_file("Rolder-1700000000000.jsonl", &"x".repeat(512));
    isolated_streamlib_home.write_log_file("camera-2-1786136667573.jsonl", &"x".repeat(1024));
    isolated_streamlib_home.write_log_file("camera-2-1786136667573.1.jsonl", &"x".repeat(1024));

    let listed = isolated_streamlib_home.run_tatolab(&["logs", "--list"]);

    assert!(listed.status.success(), "{}", standard_error_text(&listed));
    assert_eq!(
        standard_output_text(&listed),
        [
            "RUNTIME_ID-STREAM         STARTED_AT                SIZE\n",
            "camera-2                  2026-08-07T21:04:27Z      2.0 KiB\n",
            "Rolder                    2023-11-14T22:13:20Z      512 B\n",
        ]
        .concat()
    );
}

#[test]
fn reading_a_runtime_renders_its_records_as_the_runtime_mirrored_them() {
    let isolated_streamlib_home = IsolatedStreamlibHome::new();
    isolated_streamlib_home.write_log_file(
        "Rabc-1000.jsonl",
        &[
            a_log_line_with_message("started"),
            a_log_line_with_message_at_level("slow frame", "warn"),
        ]
        .concat(),
    );
    isolated_streamlib_home.write_log_file(
        "Rabc-1000.1.jsonl",
        &a_log_line_with_message_at_level("rotated", "debug"),
    );

    let everything_read = isolated_streamlib_home.run_tatolab(&["logs", "Rabc"]);
    let warnings_read = isolated_streamlib_home.run_tatolab(&["logs", "Rabc", "--level", "warn"]);

    assert!(
        everything_read.status.success(),
        "{}",
        standard_error_text(&everything_read)
    );
    assert_eq!(
        standard_output_text(&everything_read),
        format!(
            "{}\n{}\n{}\n",
            rendered_line("rotated", "DEBUG"),
            rendered_line("started", " INFO"),
            rendered_line("slow frame", " WARN")
        )
    );
    assert_eq!(standard_error_text(&everything_read), "");
    assert_eq!(
        standard_output_text(&warnings_read),
        format!("{}\n", rendered_line("slow frame", " WARN"))
    );
}

#[test]
fn a_malformed_line_is_warned_about_on_stderr_and_the_read_carries_on() {
    let isolated_streamlib_home = IsolatedStreamlibHome::new();
    isolated_streamlib_home.write_log_file(
        "Rabc-1000.jsonl",
        &[
            a_log_line_with_message("before"),
            "{ truncated\n".to_owned(),
            a_log_line_with_message("after"),
        ]
        .concat(),
    );

    let read = isolated_streamlib_home.run_tatolab(&["logs", "Rabc"]);

    assert!(read.status.success(), "{}", standard_error_text(&read));
    assert_eq!(
        standard_output_text(&read),
        format!(
            "{}\n{}\n",
            rendered_line("before", " INFO"),
            rendered_line("after", " INFO")
        )
    );
    assert!(
        standard_error_text(&read).starts_with("warning: skipping malformed JSONL line: "),
        "{}",
        standard_error_text(&read)
    );
}

/// Ctrl-C out of `--follow` is how it ends, not a failure: exit 0, not 130.
#[test]
fn follow_reads_appended_and_rotated_records_until_ctrl_c_ends_it_with_exit_zero() {
    let isolated_streamlib_home = IsolatedStreamlibHome::new();
    let active_segment_path = isolated_streamlib_home
        .write_log_file("Rabc-1000.jsonl", &a_log_line_with_message("first"));
    let following_tatolab = FollowingTatolab::spawn(
        isolated_streamlib_home.tatolab_command(&["logs", "Rabc", "--follow"]),
    );
    assert_eq!(
        following_tatolab.next_standard_output_line("the drained record"),
        rendered_line("first", " INFO")
    );

    append_to_log_file(&active_segment_path, &a_log_line_with_message("second"));
    assert_eq!(
        following_tatolab.next_standard_output_line("the appended record"),
        rendered_line("second", " INFO")
    );

    std::fs::rename(
        &active_segment_path,
        isolated_streamlib_home
            .runtime_log_directory()
            .join("Rabc-1000.1.jsonl"),
    )
    .unwrap();
    std::fs::write(&active_segment_path, a_log_line_with_message("third")).unwrap();
    assert_eq!(
        following_tatolab.next_standard_output_line("the record after the rotation"),
        rendered_line("third", " INFO")
    );

    assert_eq!(following_tatolab.interrupt_and_wait().code(), Some(0));
}

#[test]
fn follow_before_the_log_file_exists_waits_with_its_note_and_ctrl_c_ends_the_wait() {
    let isolated_streamlib_home = IsolatedStreamlibHome::new();
    let following_tatolab =
        FollowingTatolab::spawn(isolated_streamlib_home.tatolab_command(&["logs", "Rlater", "-f"]));
    assert_eq!(
        following_tatolab.next_standard_error_line("the waiting note"),
        "note: no stream log 'Rlater' yet, waiting in --follow mode..."
    );

    isolated_streamlib_home.write_log_file("Rlater-1000.jsonl", &a_log_line_with_message("booted"));
    assert_eq!(
        following_tatolab.next_standard_output_line("the first record of the new file"),
        rendered_line("booted", " INFO")
    );
    assert_eq!(following_tatolab.interrupt_and_wait().code(), Some(0));

    let still_waiting =
        FollowingTatolab::spawn(isolated_streamlib_home.tatolab_command(&["logs", "Rnever", "-f"]));
    still_waiting.next_standard_error_line("the waiting note");
    assert_eq!(still_waiting.interrupt_and_wait().code(), Some(0));
}

#[cfg(any(target_os = "linux", feature = "machine-directories-under-a-test-root"))]
mod against_an_isolated_machine {
    use serde_json::json;

    use super::common::isolated_machine_directories::IsolatedMachineDirectories;
    use super::common::runtime_log_line_fixtures::a_log_record;
    use super::common::stub_local_api_server::{StubLocalApiScript, StubStreamLogRecords};
    use super::common::tatolab_binary_run::{standard_error_text, standard_output_text};
    use super::{FollowingTatolab, rendered_line};

    fn records_saying(messages: &[&str]) -> Vec<serde_json::Value> {
        messages
            .iter()
            .map(|message| a_log_record(json!({"message": message})))
            .collect()
    }

    #[test]
    fn logs_of_a_stream_reads_its_records_from_the_runtime_page_by_page_to_the_newest() {
        let isolated_machine_directories = IsolatedMachineDirectories::new();
        let stub_local_api_server =
            isolated_machine_directories.serve_stub_local_api(StubLocalApiScript {
                stream_log_records: Some(StubStreamLogRecords {
                    records: records_saying(&["started", "running", "slowing"]),
                    page_size: 2,
                }),
                ..StubLocalApiScript::default()
            });

        let read = isolated_machine_directories.run_tatolab(&["logs", "--stream", "camera"]);

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
                json!({"stream": "camera", "after": 0}),
                json!({"stream": "camera", "after": 2}),
                json!({"stream": "camera", "after": 3}),
            ]
        );
    }

    /// Ctrl-C out of `--follow` is how it ends, not a failure: exit 0, not 130.
    #[test]
    fn logs_of_a_stream_followed_reads_on_across_pages_by_sequence_number_until_ctrl_c() {
        let isolated_machine_directories = IsolatedMachineDirectories::new();
        let stub_local_api_server =
            isolated_machine_directories.serve_stub_local_api(StubLocalApiScript {
                stream_log_records: Some(StubStreamLogRecords {
                    records: records_saying(&["first", "second", "third"]),
                    page_size: 2,
                }),
                ..StubLocalApiScript::default()
            });
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
        let afters_asked_for: Vec<u64> = stub_local_api_server
            .recorded_arguments_of("logs")
            .iter()
            .map(|logs_arguments| logs_arguments["after"].as_u64().unwrap())
            .collect();
        assert_eq!(afters_asked_for[..2], [0, 2]);
        assert!(
            afters_asked_for.windows(2).all(|pair| pair[0] <= pair[1]),
            "a follow never reads back: {afters_asked_for:?}"
        );
        assert!(afters_asked_for.contains(&3) && afters_asked_for.contains(&5));
    }

    #[test]
    fn logs_of_a_stream_refuses_a_file_name_or_list_beside_it() {
        let isolated_machine_directories = IsolatedMachineDirectories::new();

        let refused =
            isolated_machine_directories.run_tatolab(&["logs", "Rabc", "--stream", "camera"]);

        assert_eq!(refused.status.code(), Some(1));
        assert_eq!(
            standard_error_text(&refused),
            "error: `--stream` reads a loaded stream's records from the runtime, which takes no \
             RUNTIME_ID-STREAM. Drop `--stream` to read an on-disk log file instead.\n"
        );
    }
}
