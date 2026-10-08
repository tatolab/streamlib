// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `tatolab logs` run as a user runs it. The on-disk side reads the log directory under
//! `STREAMLIB_HOME`, which every floor honours; `--node` reads a registry isolated through
//! `XDG_RUNTIME_DIR`, which only Linux honours, so those tests are Linux-only.

mod common;

use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Output, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

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

fn a_log_line(message: &str, level: &str) -> String {
    format!(
        "{}\n",
        serde_json::json!({
            "schema_version": 1,
            "host_ts": 1_786_136_667_573_387_556_u64,
            "runtime_id": "Rabc",
            "source": "rust",
            "level": level,
            "message": message,
            "target": "tatolabd",
            "intercepted": false,
        })
    )
}

fn append_to_log_file(log_file_path: &Path, appended_text: &str) {
    std::fs::OpenOptions::new()
        .append(true)
        .open(log_file_path)
        .unwrap()
        .write_all(appended_text.as_bytes())
        .unwrap();
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
fn logs_is_a_served_verb() {
    let help_text = standard_output_text(&run_tatolab_reading_no_runtime_directory(&["--help"]));
    let listed_verbs: Vec<&str> = help_text
        .lines()
        .skip_while(|help_line| !help_line.starts_with("Commands:"))
        .skip(1)
        .take_while(|help_line| !help_line.trim().is_empty())
        .map(|help_line| help_line.split_whitespace().next().unwrap())
        .collect();

    assert!(listed_verbs.contains(&"logs"), "{help_text}");
}

#[test]
fn the_logs_help_names_both_modes_and_every_flag() {
    let help_text = standard_output_text(&run_tatolab_reading_no_runtime_directory(&[
        "logs", "--help",
    ]));

    for named in [
        "[RUNTIME_ID]",
        "exactly as the runtime mirrored it",
        "a running runtime's live event stream",
        "Omit with --list or --node",
        "--list",
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
        "--count <N>",
        "(--node only)",
        "--node <RUNTIME_NAME_OR_ID>",
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
fn list_refuses_the_flags_it_would_otherwise_ignore() {
    let isolated_streamlib_home = IsolatedStreamlibHome::new();

    let refused = isolated_streamlib_home.run_tatolab(&["logs", "--list", "--level", "warn"]);

    assert_eq!(refused.status.code(), Some(1));
    assert_eq!(
        standard_error_text(&refused),
        "error: `--list` enumerates the runtimes that have log files and reads none of them, so \
         it takes no --level.\n"
    );
    assert_eq!(standard_output_text(&refused), "");
}

#[test]
fn a_count_without_a_runtime_target_is_refused() {
    let isolated_streamlib_home = IsolatedStreamlibHome::new();

    let refused = isolated_streamlib_home.run_tatolab(&["logs", "Rabc", "--count", "5"]);

    assert_eq!(refused.status.code(), Some(1));
    assert!(
        standard_error_text(&refused).starts_with("error: `--count` bounds a live event-stream"),
        "{}",
        standard_error_text(&refused)
    );
}

#[test]
fn logs_without_a_runtime_id_names_list_and_node() {
    let isolated_streamlib_home = IsolatedStreamlibHome::new();

    let refused = isolated_streamlib_home.run_tatolab(&["logs"]);

    assert_eq!(refused.status.code(), Some(1));
    assert_eq!(
        standard_error_text(&refused),
        "error: missing RUNTIME_ID.\n`tatolab logs --list` enumerates the runtimes that have log \
         files, and `--node` reads a running runtime's live event stream instead.\n"
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
            "error: no log file for runtime `Rnone` in {}.\nUse `tatolab logs --list` to see the \
             runtimes that have one.\n",
            isolated_streamlib_home.runtime_log_directory().display()
        )
    );
}

#[test]
fn list_with_no_log_files_names_the_directory_it_read() {
    let isolated_streamlib_home = IsolatedStreamlibHome::new();

    let listed = isolated_streamlib_home.run_tatolab(&["logs", "--list"]);

    assert!(listed.status.success(), "{}", standard_error_text(&listed));
    assert_eq!(
        standard_output_text(&listed),
        format!(
            "(no runtime log files in {})\n",
            isolated_streamlib_home.runtime_log_directory().display()
        )
    );
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
            "RUNTIME_ID                STARTED_AT                SIZE\n",
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
            a_log_line("started", "info"),
            a_log_line("slow frame", "warn"),
        ]
        .concat(),
    );
    isolated_streamlib_home.write_log_file("Rabc-1000.1.jsonl", &a_log_line("rotated", "debug"));

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
            a_log_line("before", "info"),
            "{ truncated\n".to_owned(),
            a_log_line("after", "info"),
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
    let active_segment_path =
        isolated_streamlib_home.write_log_file("Rabc-1000.jsonl", &a_log_line("first", "info"));
    let following_tatolab = FollowingTatolab::spawn(
        isolated_streamlib_home.tatolab_command(&["logs", "Rabc", "--follow"]),
    );
    assert_eq!(
        following_tatolab.next_standard_output_line("the drained record"),
        rendered_line("first", " INFO")
    );

    append_to_log_file(&active_segment_path, &a_log_line("second", "info"));
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
    std::fs::write(&active_segment_path, a_log_line("third", "info")).unwrap();
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
        "note: no log file yet for runtime 'Rlater', waiting in --follow mode..."
    );

    isolated_streamlib_home.write_log_file("Rlater-1000.jsonl", &a_log_line("booted", "info"));
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

#[test]
fn follow_switches_to_a_newer_file_when_the_runtime_restarts() {
    let isolated_streamlib_home = IsolatedStreamlibHome::new();
    isolated_streamlib_home.write_log_file("Rabc-1000.jsonl", &a_log_line("before", "info"));
    let following_tatolab =
        FollowingTatolab::spawn(isolated_streamlib_home.tatolab_command(&["logs", "Rabc", "-f"]));
    assert_eq!(
        following_tatolab.next_standard_output_line("the pre-restart record"),
        rendered_line("before", " INFO")
    );

    isolated_streamlib_home.write_log_file("Rabc-2000.jsonl", &a_log_line("after-restart", "info"));

    assert_eq!(
        following_tatolab.next_standard_output_line("the post-restart record"),
        rendered_line("after-restart", " INFO")
    );
    assert_eq!(
        following_tatolab.next_standard_error_line("the restart note"),
        "note: runtime 'Rabc' restarted into a newer log file; switching."
    );
    assert_eq!(following_tatolab.interrupt_and_wait().code(), Some(0));
}

#[cfg(target_os = "linux")]
mod against_an_isolated_registry {
    use serde_json::json;

    use super::common::isolated_node_registry::{IsolatedNodeRegistry, a_registry_entry_named};
    use super::common::stub_local_api_server::{
        RecordedToolCall, StubLocalApiServer, StubToolAnswer,
    };
    use super::common::tatolab_binary_run::{
        run_tatolab_with_xdg_runtime_dir, standard_error_text, standard_output_text,
    };

    #[test]
    fn logs_with_a_runtime_target_reads_its_live_event_stream_through_the_local_api_socket() {
        let isolated_node_registry = IsolatedNodeRegistry::new();
        let stub_local_api_server = StubLocalApiServer::serve_answering_every_tool_call_with(
            StubToolAnswer::tool_result(r#"[{"event":"started"}]"#),
        );
        isolated_node_registry.write_registry_entry(&a_registry_entry_named(
            "Rlogs",
            "rig-logs",
            &stub_local_api_server.local_api_socket_path,
        ));

        let read = run_tatolab_with_xdg_runtime_dir(
            isolated_node_registry.xdg_runtime_dir(),
            &["logs", "--node", "rig-logs", "--count", "4"],
        );

        assert!(read.status.success(), "{}", standard_error_text(&read));
        assert_eq!(standard_output_text(&read), "[{\"event\":\"started\"}]\n");
        assert_eq!(
            stub_local_api_server.recorded_tool_calls(),
            [RecordedToolCall {
                tool_name: "logs".to_owned(),
                tool_arguments: json!({"count": 4}),
            }]
        );
    }

    #[test]
    fn logs_with_a_runtime_target_and_no_count_lets_the_tool_choose_one() {
        let isolated_node_registry = IsolatedNodeRegistry::new();
        let stub_local_api_server = StubLocalApiServer::serve_default();
        isolated_node_registry.write_registry_entry(&a_registry_entry_named(
            "Rlogs",
            "rig-logs",
            &stub_local_api_server.local_api_socket_path,
        ));

        let read = run_tatolab_with_xdg_runtime_dir(
            isolated_node_registry.xdg_runtime_dir(),
            &["logs", "--node", "Rlogs"],
        );

        assert!(read.status.success(), "{}", standard_error_text(&read));
        assert_eq!(
            stub_local_api_server.recorded_tool_calls(),
            [RecordedToolCall {
                tool_name: "logs".to_owned(),
                tool_arguments: json!({}),
            }]
        );
    }

    /// The live event-stream tool takes a count and nothing else, so a filter here would be
    /// silently ignored rather than applied.
    #[test]
    fn a_runtime_target_with_on_disk_filters_is_refused_before_any_runtime_is_reached() {
        let isolated_node_registry = IsolatedNodeRegistry::new();
        let stub_local_api_server = StubLocalApiServer::serve_default();
        isolated_node_registry.write_registry_entry(&a_registry_entry_named(
            "Rlogs",
            "rig-logs",
            &stub_local_api_server.local_api_socket_path,
        ));

        let refused = run_tatolab_with_xdg_runtime_dir(
            isolated_node_registry.xdg_runtime_dir(),
            &["logs", "--node", "rig-logs", "--level", "warn"],
        );

        assert_eq!(refused.status.code(), Some(1));
        assert_eq!(
            standard_error_text(&refused),
            "error: `--node` reads a running runtime's live event stream, which takes no \
             --level. Drop `--node` to read an on-disk log file instead.\n"
        );
        assert_eq!(
            stub_local_api_server.recorded_tool_calls(),
            [],
            "a refused flag reaches no runtime"
        );
    }

    #[test]
    fn a_runtime_target_naming_no_live_runtime_is_refused_as_the_other_verbs_refuse_it() {
        let isolated_node_registry = IsolatedNodeRegistry::new();

        let refused = run_tatolab_with_xdg_runtime_dir(
            isolated_node_registry.xdg_runtime_dir(),
            &["logs", "--node", "rig-logs"],
        );

        assert_eq!(refused.status.code(), Some(1));
        assert_eq!(
            standard_error_text(&refused),
            "error: no running runtime found on this machine.\nStart one with `tatolab run`.\n"
        );
    }
}
