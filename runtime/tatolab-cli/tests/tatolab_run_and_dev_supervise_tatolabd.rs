// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

const BOUNDED_WAIT: Duration = Duration::from_secs(20);
const POLL_INTERVAL: Duration = Duration::from_millis(10);
const COMPILE_ENTRY_MODULE: &str = "tatolab.stream._project_stream_compile_entry";

/// A fake `tatolabd`: records its argv, the two engine variables, its cwd, its process group and
/// a copy of its graph file under `<control>/tatolabd_runs/<pid>/`, logs each INT/TERM/HUP, and
/// exits `tatolabd_exit_delay_seconds` after `tatolabd_signals_before_exit` signals (default 1)
/// with `tatolabd_exit_code`.
const FAKE_TATOLABD_SCRIPT: &str = r#"#!/bin/sh
control_directory='@CONTROL@'
run_directory="$control_directory/tatolabd_runs/$$"
mkdir -p "$run_directory"
for argument in "$@"; do printf '%s\n' "$argument" >> "$run_directory/argv"; done
previous_argument=''
for argument in "$@"; do
  if [ "$previous_argument" = "--stream-graph" ]; then cp "$argument" "$run_directory/graph.json"; fi
  previous_argument="$argument"
done
printf '%s' "${STREAMLIB_APP_DIRECTORY-<unset>}" > "$run_directory/app_directory"
printf '%s' "${STREAMLIB_RUNTIME_NAME-<unset>}" > "$run_directory/runtime_name"
pwd -P | tr -d '\n' > "$run_directory/cwd"
ps -o pgid= -p $$ | tr -d ' \n' > "$run_directory/pgid"
received_signal_count=0
record_signal() {
  printf '%s\n' "$1" >> "$run_directory/signals"
  received_signal_count=$((received_signal_count + 1))
}
trap 'record_signal INT' INT
trap 'record_signal TERM' TERM
trap 'record_signal HUP' HUP
printf '%s\n' "$$" >> "$control_directory/tatolabd_starts"
if [ -f "$control_directory/tatolabd_dies_by_sigterm" ]; then
  trap - TERM
  kill -TERM $$
fi
if [ -f "$control_directory/tatolabd_exits_on_its_own" ]; then
  exit_code=$(cat "$control_directory/tatolabd_exits_on_its_own")
  rm -f "$control_directory/tatolabd_exits_on_its_own"
  printf '%s' "$exit_code" > "$run_directory/exit_code"
  exit "$exit_code"
fi
signals_before_exit=$(cat "$control_directory/tatolabd_signals_before_exit" 2>/dev/null || echo 1)
while [ "$received_signal_count" -lt "$signals_before_exit" ]; do sleep 0.02; done
if [ -f "$control_directory/tatolabd_exit_delay_seconds" ]; then sleep "$(cat "$control_directory/tatolabd_exit_delay_seconds")"; fi
exit_code=$(cat "$control_directory/tatolabd_exit_code" 2>/dev/null || echo 0)
printf '%s' "$exit_code" > "$run_directory/exit_code"
exit "$exit_code"
"#;

/// A fake venv interpreter: answers the `tatolab.stream` probe and emulates the compile entry,
/// logging each compile's cwd and arguments to `<control>/compile_invocations` and its pid to
/// `<control>/compile_process_ids`.
const FAKE_PROJECT_PYTHON_SCRIPT: &str = r#"#!/bin/sh
control_directory='@CONTROL@'
if [ "$1" = "-I" ] && [ "$2" = "-c" ]; then
  if [ -f "$control_directory/probe_fails" ]; then exit 1; fi
  exit 0
fi
{
  pwd -P
  for argument in "$@"; do printf '%s\n' "$argument"; done
  printf '%s\n' '--end-of-invocation--'
} >> "$control_directory/compile_invocations"
printf '%s\n' "$$" >> "$control_directory/compile_process_ids"
if [ -f "$control_directory/compile_delay_seconds" ]; then sleep "$(cat "$control_directory/compile_delay_seconds")"; fi
if [ -f "$control_directory/compile_stderr" ]; then cat "$control_directory/compile_stderr" >&2; fi
if [ -f "$control_directory/compile_leaves_a_process_holding_its_stdout" ]; then sleep 30 & fi
exit_code=$(cat "$control_directory/compile_exit_code" 2>/dev/null || echo 0)
if [ "$exit_code" = 0 ]; then cat "$control_directory/compile_document.json"; fi
exit "$exit_code"
"#;

struct CompileInvocation {
    working_directory: PathBuf,
    arguments: Vec<String>,
}

struct FakeTatolabdRun {
    argv: Vec<String>,
    app_directory: String,
    runtime_name: String,
    working_directory: PathBuf,
    process_group_id: u32,
    process_id: u32,
    stream_graph: Value,
    stream_graph_text: String,
    signals: Vec<String>,
    exit_code: Option<String>,
}

struct AttachedTatolabdTestbed {
    testbed_root: tempfile::TempDir,
}

struct RunningTatolab {
    tatolab_child: Child,
}

impl Drop for RunningTatolab {
    fn drop(&mut self) {
        let _ = self.tatolab_child.kill();
        let _ = self.tatolab_child.wait();
    }
}

impl RunningTatolab {
    fn process_id(&self) -> libc::pid_t {
        self.tatolab_child.id() as libc::pid_t
    }

    fn send_signal(&self, signal_to_send: libc::c_int) {
        // SAFETY: `kill` reads no memory; the pid is this test's unreaped child.
        assert_eq!(unsafe { libc::kill(self.process_id(), signal_to_send) }, 0);
    }

    fn wait_for_exit(&mut self) -> ExitStatus {
        let started_waiting = Instant::now();
        loop {
            if let Some(exit_status) = self.tatolab_child.try_wait().unwrap() {
                return exit_status;
            }
            assert!(
                started_waiting.elapsed() < BOUNDED_WAIT,
                "tatolab did not exit"
            );
            std::thread::sleep(POLL_INTERVAL);
        }
    }

    fn is_still_running(&mut self) -> bool {
        self.tatolab_child.try_wait().unwrap().is_none()
    }
}

fn write_executable_script(script_path: &Path, script_text: &str) {
    fs::create_dir_all(script_path.parent().unwrap()).unwrap();
    fs::write(script_path, script_text).unwrap();
    fs::set_permissions(script_path, fs::Permissions::from_mode(0o755)).unwrap();
}

fn wait_until(what_is_awaited: &str, mut condition: impl FnMut() -> bool) {
    let started_waiting = Instant::now();
    while !condition() {
        assert!(
            started_waiting.elapsed() < BOUNDED_WAIT,
            "timed out waiting for {what_is_awaited}"
        );
        std::thread::sleep(POLL_INTERVAL);
    }
}

fn spawn_retrying_a_busy_executable(command: &mut Command) -> Child {
    let started_spawning = Instant::now();
    loop {
        match command.spawn() {
            Ok(spawned_child) => return spawned_child,
            Err(spawn_failure)
                if spawn_failure.raw_os_error() == Some(libc::ETXTBSY)
                    && started_spawning.elapsed() < BOUNDED_WAIT =>
            {
                std::thread::sleep(POLL_INTERVAL);
            }
            Err(spawn_failure) => panic!("cannot spawn tatolab: {spawn_failure}"),
        }
    }
}

impl AttachedTatolabdTestbed {
    fn new() -> Self {
        let testbed = Self {
            testbed_root: tempfile::tempdir().unwrap(),
        };
        let control_directory = testbed.control_directory();
        fs::create_dir_all(&control_directory).unwrap();
        fs::create_dir_all(testbed.bin_directory()).unwrap();
        fs::copy(env!("CARGO_BIN_EXE_tatolab"), testbed.tatolab_executable()).unwrap();
        write_executable_script(
            &testbed.bin_directory().join("tatolabd"),
            &FAKE_TATOLABD_SCRIPT.replace("@CONTROL@", control_directory.to_str().unwrap()),
        );
        write_executable_script(
            &testbed.project_directory().join(".venv/bin/python"),
            &FAKE_PROJECT_PYTHON_SCRIPT.replace("@CONTROL@", control_directory.to_str().unwrap()),
        );
        fs::write(
            testbed.project_directory().join("stream.py"),
            "# stream v1\n",
        )
        .unwrap();
        testbed.set_compile_document(&json!({"stream": "probe", "nodes": []}));
        testbed
    }

    fn root_directory(&self) -> PathBuf {
        self.testbed_root.path().canonicalize().unwrap()
    }

    fn control_directory(&self) -> PathBuf {
        self.root_directory().join("control")
    }

    fn bin_directory(&self) -> PathBuf {
        self.root_directory().join("bin")
    }

    fn tatolab_executable(&self) -> PathBuf {
        self.bin_directory().join("tatolab")
    }

    fn project_directory(&self) -> PathBuf {
        self.root_directory().join("project")
    }

    fn project_interpreter(&self) -> PathBuf {
        self.project_directory().join(".venv/bin/python")
    }

    fn compiled_project_directory(&self) -> PathBuf {
        self.root_directory().join("compiled-project-directory")
    }

    fn tatolab_stderr_path(&self) -> PathBuf {
        self.root_directory().join("tatolab.stderr")
    }

    fn set_control(&self, control_name: &str, control_value: &str) {
        fs::write(self.control_directory().join(control_name), control_value).unwrap();
    }

    fn set_compile_document(&self, stream_graph: &Value) {
        let compile_document = json!({
            "stream_graph": stream_graph,
            "project_directory": self.compiled_project_directory(),
        });
        self.set_control("compile_document.json", &compile_document.to_string());
    }

    fn set_compile_document_text(&self, stream_graph_text: &str) {
        let compile_document_text = format!(
            r#"{{"stream_graph": {stream_graph_text}, "project_directory": {}}}"#,
            json!(self.compiled_project_directory())
        );
        self.set_control("compile_document.json", &compile_document_text);
    }

    fn start_tatolab(
        &self,
        working_directory: &Path,
        tatolab_arguments: &[&str],
    ) -> RunningTatolab {
        let tatolab_stderr = fs::File::create(self.tatolab_stderr_path()).unwrap();
        let mut tatolab_command = Command::new(self.tatolab_executable());
        tatolab_command
            .args(tatolab_arguments)
            .current_dir(working_directory)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(tatolab_stderr)
            .env_remove("STREAMLIB_RUNTIME_NAME")
            .env_remove("STREAMLIB_APP_DIRECTORY");
        RunningTatolab {
            tatolab_child: spawn_retrying_a_busy_executable(&mut tatolab_command),
        }
    }

    fn run_tatolab_to_exit(&self, tatolab_arguments: &[&str]) -> (ExitStatus, String) {
        let mut running_tatolab = self.start_tatolab(&self.project_directory(), tatolab_arguments);
        let exit_status = running_tatolab.wait_for_exit();
        (exit_status, self.tatolab_stderr())
    }

    fn tatolab_stderr(&self) -> String {
        fs::read_to_string(self.tatolab_stderr_path()).unwrap_or_default()
    }

    fn compile_invocations(&self) -> Vec<CompileInvocation> {
        let Ok(invocation_log) =
            fs::read_to_string(self.control_directory().join("compile_invocations"))
        else {
            return Vec::new();
        };
        let mut compile_invocations = Vec::new();
        let mut invocation_lines: Vec<String> = Vec::new();
        for invocation_log_line in invocation_log.lines() {
            if invocation_log_line == "--end-of-invocation--" {
                compile_invocations.push(CompileInvocation {
                    working_directory: PathBuf::from(&invocation_lines[0]),
                    arguments: invocation_lines[1..].to_vec(),
                });
                invocation_lines.clear();
            } else {
                invocation_lines.push(invocation_log_line.to_owned());
            }
        }
        compile_invocations
    }

    fn compile_process_ids(&self) -> Vec<u32> {
        fs::read_to_string(self.control_directory().join("compile_process_ids"))
            .unwrap_or_default()
            .lines()
            .map(|process_id_line| process_id_line.parse().unwrap())
            .collect()
    }

    fn started_tatolabd_process_ids(&self) -> Vec<u32> {
        fs::read_to_string(self.control_directory().join("tatolabd_starts"))
            .unwrap_or_default()
            .lines()
            .map(|process_id_line| process_id_line.parse().unwrap())
            .collect()
    }

    fn tatolabd_run(&self, process_id: u32) -> FakeTatolabdRun {
        let run_directory = self
            .control_directory()
            .join("tatolabd_runs")
            .join(process_id.to_string());
        let read_run_file =
            |run_file: &str| fs::read_to_string(run_directory.join(run_file)).unwrap_or_default();
        FakeTatolabdRun {
            argv: read_run_file("argv").lines().map(str::to_owned).collect(),
            app_directory: read_run_file("app_directory"),
            runtime_name: read_run_file("runtime_name"),
            working_directory: PathBuf::from(read_run_file("cwd")),
            process_group_id: read_run_file("pgid").parse().unwrap(),
            process_id,
            stream_graph: serde_json::from_str(&read_run_file("graph.json")).unwrap(),
            stream_graph_text: read_run_file("graph.json"),
            signals: read_run_file("signals")
                .lines()
                .map(str::to_owned)
                .collect(),
            exit_code: run_directory
                .join("exit_code")
                .exists()
                .then(|| read_run_file("exit_code")),
        }
    }

    fn wait_for_tatolabd_start_count(&self, expected_start_count: usize) -> Vec<FakeTatolabdRun> {
        wait_until(&format!("{expected_start_count} tatolabd start(s)"), || {
            self.started_tatolabd_process_ids().len() >= expected_start_count
        });
        self.started_tatolabd_process_ids()
            .into_iter()
            .map(|process_id| self.tatolabd_run(process_id))
            .collect()
    }

    fn wait_for_tatolab_stderr_to_contain(&self, expected_text: &str) {
        wait_until(
            &format!("tatolab's stderr to say {expected_text:?}"),
            || self.tatolab_stderr().contains(expected_text),
        );
    }

    fn clear_control(&self, control_name: &str) {
        let _ = fs::remove_file(self.control_directory().join(control_name));
    }

    fn edit_project_file(&self, path_in_project: &str, new_contents: &str) {
        let edited_file = self.project_directory().join(path_in_project);
        fs::create_dir_all(edited_file.parent().unwrap()).unwrap();
        fs::write(edited_file, new_contents).unwrap();
    }
}

fn compile_arguments(verb: &str, forwarded_arguments: &[&str]) -> Vec<String> {
    ["-I", "-m", COMPILE_ENTRY_MODULE, "--verb", verb]
        .iter()
        .chain(forwarded_arguments)
        .map(|argument| (*argument).to_owned())
        .collect()
}

#[test]
fn run_refuses_a_project_without_a_venv_naming_uv_sync() {
    let testbed = AttachedTatolabdTestbed::new();
    fs::remove_dir_all(testbed.project_directory().join(".venv")).unwrap();
    let (exit_status, tatolab_stderr) = testbed.run_tatolab_to_exit(&["run"]);
    assert_eq!(exit_status.code(), Some(1));
    assert_eq!(
        tatolab_stderr,
        format!(
            "error: no virtual environment at {}/.venv — run `uv sync` in {} first\n",
            testbed.project_directory().display(),
            testbed.project_directory().display()
        )
    );
    assert!(testbed.started_tatolabd_process_ids().is_empty());
}

#[test]
fn run_refuses_a_venv_that_cannot_import_tatolab_stream() {
    let testbed = AttachedTatolabdTestbed::new();
    testbed.set_control("probe_fails", "");
    let (exit_status, tatolab_stderr) = testbed.run_tatolab_to_exit(&["run"]);
    assert_eq!(exit_status.code(), Some(1));
    assert!(tatolab_stderr.starts_with("error: "), "{tatolab_stderr}");
    assert!(
        tatolab_stderr.contains("tatolab-stream"),
        "{tatolab_stderr}"
    );
    assert!(tatolab_stderr.contains("uv sync"), "{tatolab_stderr}");
    assert!(testbed.compile_invocations().is_empty());
}

#[test]
fn run_refuses_a_missing_tatolabd_naming_build_runtime() {
    let testbed = AttachedTatolabdTestbed::new();
    fs::remove_file(testbed.bin_directory().join("tatolabd")).unwrap();
    let (exit_status, tatolab_stderr) = testbed.run_tatolab_to_exit(&["run"]);
    assert_eq!(exit_status.code(), Some(1));
    assert_eq!(
        tatolab_stderr,
        format!(
            "error: no tatolabd beside {} — build the runtime unit with `cargo xtask build-runtime`\n",
            testbed.tatolab_executable().display()
        )
    );
}

#[test]
fn run_compiles_in_the_venv_and_hands_tatolabd_the_graph_project_and_interpreter() {
    let testbed = AttachedTatolabdTestbed::new();
    let stream_graph = json!({"stream": "probe", "nodes": [{"name": "a"}], "links": []});
    testbed.set_compile_document(&stream_graph);
    testbed.set_control("compile_delay_seconds", "0.2");
    let mut running_tatolab = testbed.start_tatolab(&testbed.project_directory(), &["run"]);
    let tatolabd_runs = testbed.wait_for_tatolabd_start_count(1);
    let tatolabd_run = &tatolabd_runs[0];

    let compile_invocations = testbed.compile_invocations();
    assert_eq!(compile_invocations.len(), 1);
    assert_eq!(
        compile_invocations[0].working_directory,
        testbed.project_directory()
    );
    assert_eq!(
        compile_invocations[0].arguments,
        compile_arguments("run", &[])
    );

    assert_eq!(tatolabd_run.argv.len(), 6);
    assert_eq!(tatolabd_run.argv[0], "--stream-graph");
    assert_eq!(
        tatolabd_run.argv[2..],
        [
            "--project".to_owned(),
            testbed.compiled_project_directory().display().to_string(),
            "--interpreter".to_owned(),
            testbed.project_interpreter().display().to_string(),
        ]
    );
    assert_eq!(tatolabd_run.stream_graph, stream_graph);
    assert_eq!(
        tatolabd_run.app_directory,
        testbed.project_directory().display().to_string()
    );
    assert_eq!(tatolabd_run.runtime_name, "<unset>");
    assert_eq!(tatolabd_run.working_directory, testbed.project_directory());
    assert_eq!(tatolabd_run.process_group_id, tatolabd_run.process_id);
    assert_ne!(
        tatolabd_run.process_group_id,
        // SAFETY: `getpgid` reads no memory; the pid is this test's unreaped child.
        unsafe { libc::getpgid(running_tatolab.process_id()) } as u32
    );

    running_tatolab.send_signal(libc::SIGINT);
    let exit_status = running_tatolab.wait_for_exit();
    assert_eq!(exit_status.code(), Some(0));
    assert!(
        !Path::new(&tatolabd_run.argv[1]).exists(),
        "the graph file outlived tatolab"
    );
}

#[test]
fn run_hands_tatolabd_the_compiled_graph_byte_for_byte() {
    let testbed = AttachedTatolabdTestbed::new();
    let stream_graph_text = r#"{"stream": "probe", "nodes": [{"name": "a", "config": {"wider_than_u64": 18446744073709551616, "tenth": 0.1}}], "links": []}"#;
    testbed.set_compile_document_text(stream_graph_text);
    let mut running_tatolab = testbed.start_tatolab(&testbed.project_directory(), &["run"]);
    let tatolabd_runs = testbed.wait_for_tatolabd_start_count(1);

    assert_eq!(tatolabd_runs[0].stream_graph_text, stream_graph_text);

    running_tatolab.send_signal(libc::SIGINT);
    assert_eq!(running_tatolab.wait_for_exit().code(), Some(0));
}

/// A fake `tatolabd` in Python, which, unlike the shell, starts with the signal mask its parent
/// handed it: it records the signals blocked at its start and exits 0.
const SIGNAL_MASK_RECORDING_TATOLABD_SCRIPT: &str = r#"#!/usr/bin/env python3
import signal
blocked_at_start = signal.pthread_sigmask(signal.SIG_BLOCK, [])
with open('@CONTROL@/tatolabd_signals_blocked_at_start', 'w') as record:
    record.write(' '.join(sorted(blocked_signal.name for blocked_signal in blocked_at_start)))
"#;

#[test]
fn tatolabd_starts_with_none_of_the_forwarded_signals_blocked() {
    let testbed = AttachedTatolabdTestbed::new();
    let control_directory = testbed.control_directory();
    write_executable_script(
        &testbed.bin_directory().join("tatolabd"),
        &SIGNAL_MASK_RECORDING_TATOLABD_SCRIPT
            .replace("@CONTROL@", control_directory.to_str().unwrap()),
    );

    let (exit_status, tatolab_stderr) = testbed.run_tatolab_to_exit(&["run"]);
    assert_eq!(exit_status.code(), Some(0), "{tatolab_stderr}");

    let signals_blocked_at_start =
        fs::read_to_string(control_directory.join("tatolabd_signals_blocked_at_start")).unwrap();
    for forwarded_signal_name in ["SIGINT", "SIGTERM", "SIGHUP"] {
        assert!(
            !signals_blocked_at_start
                .split_whitespace()
                .any(|blocked_signal_name| blocked_signal_name == forwarded_signal_name),
            "tatolabd started with {forwarded_signal_name} blocked ({signals_blocked_at_start}), \
             so no forwarded {forwarded_signal_name} could stop it"
        );
    }
}

const SIGNAL_DISPOSITION_RECORDING_TATOLABD_SCRIPT: &str = r#"#!/usr/bin/env python3
import signal
with open('@CONTROL@/tatolabd_signals_ignored_at_start', 'w') as record:
    record.write(' '.join(sorted(
        forwarded_signal.name
        for forwarded_signal in (signal.SIGINT, signal.SIGTERM, signal.SIGHUP)
        if signal.getsignal(forwarded_signal) == signal.SIG_IGN
    )))
"#;

#[test]
fn tatolab_resets_an_inherited_ignored_interrupt_and_terminate_and_keeps_an_inherited_ignored_hangup()
 {
    let testbed = AttachedTatolabdTestbed::new();
    let control_directory = testbed.control_directory();
    write_executable_script(
        &testbed.bin_directory().join("tatolabd"),
        &SIGNAL_DISPOSITION_RECORDING_TATOLABD_SCRIPT
            .replace("@CONTROL@", control_directory.to_str().unwrap()),
    );

    let tatolab_stderr = fs::File::create(testbed.tatolab_stderr_path()).unwrap();
    let mut tatolab_command = Command::new(testbed.tatolab_executable());
    tatolab_command
        .arg("run")
        .current_dir(testbed.project_directory())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(tatolab_stderr)
        .env_remove("STREAMLIB_RUNTIME_NAME")
        .env_remove("STREAMLIB_APP_DIRECTORY");
    // SAFETY: the closure only calls `signal`, which is async-signal-safe after fork.
    unsafe {
        tatolab_command.pre_exec(|| {
            for ignored_signal in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
                if libc::signal(ignored_signal, libc::SIG_IGN) == libc::SIG_ERR {
                    return Err(std::io::Error::last_os_error());
                }
            }
            Ok(())
        });
    }
    let mut running_tatolab = RunningTatolab {
        tatolab_child: spawn_retrying_a_busy_executable(&mut tatolab_command),
    };
    let exit_status = running_tatolab.wait_for_exit();
    assert_eq!(exit_status.code(), Some(0), "{}", testbed.tatolab_stderr());

    let signals_ignored_at_start =
        fs::read_to_string(control_directory.join("tatolabd_signals_ignored_at_start")).unwrap();
    assert_eq!(
        signals_ignored_at_start, "SIGHUP",
        "tatolab must reset an inherited ignored SIGINT and SIGTERM, since an ignored signal \
         never reaches its sigwait listener on macOS, and keep an inherited ignored SIGHUP"
    );
}

#[test]
fn run_forwards_target_and_flags_verbatim_and_runtime_name_as_the_engine_variable() {
    let testbed = AttachedTatolabdTestbed::new();
    let mut running_tatolab = testbed.start_tatolab(
        &testbed.root_directory(),
        &[
            "run",
            "custom.py:main",
            "-f",
            "other.py",
            "--dir",
            "project",
            "--name",
            "renamed",
            "--runtime-name",
            "probe-runtime",
        ],
    );
    let tatolabd_runs = testbed.wait_for_tatolabd_start_count(1);
    let compile_invocations = testbed.compile_invocations();
    assert_eq!(
        compile_invocations[0].arguments,
        compile_arguments(
            "run",
            &[
                "custom.py:main",
                "-f",
                "other.py",
                "--dir",
                "project",
                "--name",
                "renamed"
            ]
        )
    );
    assert_eq!(
        compile_invocations[0].working_directory,
        testbed.project_directory()
    );
    assert_eq!(tatolabd_runs[0].runtime_name, "probe-runtime");
    assert_eq!(
        tatolabd_runs[0].app_directory,
        testbed.project_directory().display().to_string()
    );
    assert_eq!(
        tatolabd_runs[0].working_directory,
        testbed.project_directory()
    );
    assert_eq!(
        tatolabd_runs[0].argv[5],
        testbed.project_interpreter().display().to_string()
    );
    running_tatolab.send_signal(libc::SIGINT);
    assert_eq!(running_tatolab.wait_for_exit().code(), Some(0));
}

#[test]
fn a_failed_compile_ends_run_with_its_exit_code_and_starts_no_tatolabd() {
    let testbed = AttachedTatolabdTestbed::new();
    testbed.set_control("compile_exit_code", "3");
    testbed.set_control("compile_stderr", "Traceback: the probe stream raised\n");
    let (exit_status, tatolab_stderr) = testbed.run_tatolab_to_exit(&["run"]);
    assert_eq!(exit_status.code(), Some(3));
    assert_eq!(tatolab_stderr, "Traceback: the probe stream raised\n");
    assert!(testbed.started_tatolabd_process_ids().is_empty());
}

#[test]
fn a_failed_compile_whose_child_still_holds_its_stdout_ends_run_without_waiting_on_it() {
    let testbed = AttachedTatolabdTestbed::new();
    testbed.set_control("compile_exit_code", "1");
    testbed.set_control("compile_leaves_a_process_holding_its_stdout", "");
    let run_started = std::time::Instant::now();
    let (exit_status, _) = testbed.run_tatolab_to_exit(&["run"]);
    assert_eq!(exit_status.code(), Some(1));
    assert!(
        run_started.elapsed() < std::time::Duration::from_secs(10),
        "run waited {:?} on a process the failed compile left holding its stdout",
        run_started.elapsed()
    );
    assert!(testbed.started_tatolabd_process_ids().is_empty());
}

#[test]
fn an_app_that_exits_zero_while_it_compiles_ends_run_cleanly_and_starts_no_tatolabd() {
    let testbed = AttachedTatolabdTestbed::new();
    testbed.set_control("compile_document.json", "");
    let (exit_status, tatolab_stderr) = testbed.run_tatolab_to_exit(&["run"]);
    assert_eq!(exit_status.code(), Some(0));
    assert_eq!(tatolab_stderr, "");
    assert!(testbed.started_tatolabd_process_ids().is_empty());
}

#[test]
fn a_signal_during_the_compile_ends_run_with_128_plus_it_and_starts_no_tatolabd() {
    let testbed = AttachedTatolabdTestbed::new();
    testbed.set_control("compile_delay_seconds", "10");
    let mut running_tatolab = testbed.start_tatolab(&testbed.project_directory(), &["run"]);
    wait_until("the compile to start", || {
        !testbed.compile_invocations().is_empty()
    });
    running_tatolab.send_signal(libc::SIGTERM);
    assert_eq!(
        running_tatolab.wait_for_exit().code(),
        Some(128 + libc::SIGTERM)
    );
    assert!(testbed.started_tatolabd_process_ids().is_empty());
}

#[test]
fn run_forwards_sigint_sigterm_and_sighup_once_each_and_exits_with_tatolabd_code() {
    let testbed = AttachedTatolabdTestbed::new();
    testbed.set_control("tatolabd_signals_before_exit", "3");
    testbed.set_control("tatolabd_exit_code", "130");
    let mut running_tatolab = testbed.start_tatolab(&testbed.project_directory(), &["run"]);
    let tatolabd_process_id = testbed.wait_for_tatolabd_start_count(1)[0].process_id;

    for (signal_to_send, expected_signal_log) in [
        (libc::SIGINT, vec!["INT"]),
        (libc::SIGTERM, vec!["INT", "TERM"]),
        (libc::SIGHUP, vec!["INT", "TERM", "HUP"]),
    ] {
        running_tatolab.send_signal(signal_to_send);
        wait_until(&format!("tatolabd to log {expected_signal_log:?}"), || {
            testbed.tatolabd_run(tatolabd_process_id).signals.len() >= expected_signal_log.len()
        });
        assert_eq!(
            testbed.tatolabd_run(tatolabd_process_id).signals,
            expected_signal_log
        );
    }
    let exit_status = running_tatolab.wait_for_exit();
    assert_eq!(exit_status.code(), Some(130));
    assert_eq!(
        testbed.tatolabd_run(tatolabd_process_id).signals,
        ["INT", "TERM", "HUP"]
    );
    assert_eq!(testbed.started_tatolabd_process_ids().len(), 1);
}

#[test]
fn run_exits_with_tatolabd_own_exit_code() {
    let testbed = AttachedTatolabdTestbed::new();
    testbed.set_control("tatolabd_exits_on_its_own", "7");
    let (exit_status, _) = testbed.run_tatolab_to_exit(&["run"]);
    assert_eq!(exit_status.code(), Some(7));
}

#[test]
fn run_exits_128_plus_the_signal_that_killed_tatolabd() {
    let testbed = AttachedTatolabdTestbed::new();
    testbed.set_control("tatolabd_dies_by_sigterm", "");
    let (exit_status, _) = testbed.run_tatolab_to_exit(&["run"]);
    assert_eq!(exit_status.code(), Some(128 + libc::SIGTERM));
    assert_eq!(exit_status.signal(), None);
}

#[test]
fn dev_restarts_tatolabd_on_the_new_graph_after_an_edit() {
    let testbed = AttachedTatolabdTestbed::new();
    let mut running_tatolab = testbed.start_tatolab(&testbed.project_directory(), &["dev"]);
    let first_tatolabd_process_id = testbed.wait_for_tatolabd_start_count(1)[0].process_id;
    assert_eq!(
        testbed.compile_invocations()[0].arguments,
        compile_arguments("dev", &[])
    );

    let edited_stream_graph = json!({"stream": "probe", "nodes": [{"name": "edited"}]});
    testbed.set_compile_document(&edited_stream_graph);
    testbed.edit_project_file("nodes/inverting_effect.py", "# edited node\n");
    let tatolabd_runs = testbed.wait_for_tatolabd_start_count(2);

    let first_tatolabd_run = testbed.tatolabd_run(first_tatolabd_process_id);
    assert_eq!(first_tatolabd_run.signals, ["INT"]);
    assert!(first_tatolabd_run.exit_code.is_some());
    assert_eq!(tatolabd_runs[1].stream_graph, edited_stream_graph);
    assert_eq!(testbed.compile_invocations().len(), 2);
    assert!(running_tatolab.is_still_running());

    running_tatolab.send_signal(libc::SIGINT);
    assert_eq!(running_tatolab.wait_for_exit().code(), Some(0));
    assert_eq!(
        testbed.tatolabd_run(tatolabd_runs[1].process_id).signals,
        ["INT"]
    );
    assert_eq!(testbed.started_tatolabd_process_ids().len(), 2);
}

#[test]
fn dev_keeps_the_running_tatolabd_when_the_recompile_fails() {
    let testbed = AttachedTatolabdTestbed::new();
    let mut running_tatolab = testbed.start_tatolab(&testbed.project_directory(), &["dev"]);
    let tatolabd_process_id = testbed.wait_for_tatolabd_start_count(1)[0].process_id;

    testbed.set_control("compile_exit_code", "1");
    testbed.set_control("compile_stderr", "SyntaxError: the probe edit\n");
    testbed.edit_project_file("stream.py", "# stream v2, broken\n");
    testbed.wait_for_tatolab_stderr_to_contain(
        "tatolab dev: kept the running stream — fix the error and save again",
    );
    assert!(
        testbed
            .tatolab_stderr()
            .contains("SyntaxError: the probe edit")
    );
    assert!(testbed.tatolabd_run(tatolabd_process_id).signals.is_empty());
    assert!(
        testbed
            .tatolabd_run(tatolabd_process_id)
            .exit_code
            .is_none()
    );
    assert!(running_tatolab.is_still_running());

    running_tatolab.send_signal(libc::SIGTERM);
    assert_eq!(running_tatolab.wait_for_exit().code(), Some(0));
    assert_eq!(testbed.tatolabd_run(tatolabd_process_id).signals, ["TERM"]);
    assert_eq!(testbed.started_tatolabd_process_ids().len(), 1);
}

#[test]
fn dev_waits_for_an_edit_after_tatolabd_exits_on_its_own() {
    let testbed = AttachedTatolabdTestbed::new();
    testbed.set_control("tatolabd_exits_on_its_own", "7");
    let mut running_tatolab = testbed.start_tatolab(&testbed.project_directory(), &["dev"]);
    testbed.wait_for_tatolabd_start_count(1);
    testbed.wait_for_tatolab_stderr_to_contain(
        "tatolab dev: tatolabd exited with exit code 7 — waiting for the next edit",
    );
    assert!(running_tatolab.is_still_running());
    assert_eq!(testbed.compile_invocations().len(), 1);

    testbed.edit_project_file("stream.py", "# stream v2\n");
    let tatolabd_runs = testbed.wait_for_tatolabd_start_count(2);
    assert_eq!(testbed.compile_invocations().len(), 2);

    running_tatolab.send_signal(libc::SIGINT);
    assert_eq!(running_tatolab.wait_for_exit().code(), Some(0));
    assert_eq!(
        testbed.tatolabd_run(tatolabd_runs[1].process_id).signals,
        ["INT"]
    );
}

#[test]
fn dev_ignores_edits_under_venvs_caches_and_dot_directories() {
    let testbed = AttachedTatolabdTestbed::new();
    let mut running_tatolab = testbed.start_tatolab(&testbed.project_directory(), &["dev"]);
    testbed.wait_for_tatolabd_start_count(1);

    testbed.edit_project_file(".venv/lib/site-packages/edited.py", "# ignored\n");
    testbed.edit_project_file("__pycache__/edited.py", "# ignored\n");
    testbed.edit_project_file("nodes/__pycache__/edited.py", "# ignored\n");
    testbed.edit_project_file("venv/edited.py", "# ignored\n");
    testbed.edit_project_file("other-environment/pyvenv.cfg", "home = /usr\n");
    testbed.edit_project_file("other-environment/lib/edited.py", "# ignored\n");
    testbed.edit_project_file(".hidden/edited.py", "# ignored\n");
    testbed.edit_project_file("notes.txt", "not a watched file\n");
    // Four scan intervals: long enough for a watched edit to have settled and recompiled.
    std::thread::sleep(Duration::from_millis(1000));
    assert_eq!(testbed.compile_invocations().len(), 1);
    assert_eq!(testbed.started_tatolabd_process_ids().len(), 1);

    testbed.edit_project_file("pyproject.toml", "[project]\nname = \"edited\"\n");
    testbed.wait_for_tatolabd_start_count(2);
    assert_eq!(testbed.compile_invocations().len(), 2);

    running_tatolab.send_signal(libc::SIGINT);
    assert_eq!(running_tatolab.wait_for_exit().code(), Some(0));
}

#[test]
fn dev_reports_a_failed_teardown_and_folds_a_newer_graph_into_the_pending_restart() {
    let testbed = AttachedTatolabdTestbed::new();
    let mut running_tatolab = testbed.start_tatolab(&testbed.project_directory(), &["dev"]);
    let first_tatolabd_process_id = testbed.wait_for_tatolabd_start_count(1)[0].process_id;

    testbed.set_control("tatolabd_exit_delay_seconds", "2.5");
    testbed.set_control("tatolabd_exit_code", "124");
    testbed.set_compile_document(&json!({"stream": "probe", "nodes": [{"name": "second"}]}));
    testbed.edit_project_file("stream.py", "# stream v2\n");
    testbed.wait_for_tatolab_stderr_to_contain("tatolab dev: restarting the stream");

    let newest_stream_graph = json!({"stream": "probe", "nodes": [{"name": "third"}]});
    testbed.set_compile_document(&newest_stream_graph);
    testbed.edit_project_file("stream.py", "# stream v3, a longer edit\n");
    wait_until("the third compile", || {
        testbed.compile_invocations().len() >= 3
    });
    let tatolabd_runs = testbed.wait_for_tatolabd_start_count(2);
    testbed.clear_control("tatolabd_exit_delay_seconds");
    testbed.clear_control("tatolabd_exit_code");

    assert_eq!(
        testbed.tatolabd_run(first_tatolabd_process_id).signals,
        ["INT"]
    );
    assert_eq!(tatolabd_runs[1].stream_graph, newest_stream_graph);
    assert_eq!(testbed.compile_invocations().len(), 3);
    assert!(
        testbed
            .tatolab_stderr()
            .contains("tatolab dev: the previous stream exited with exit code 124"),
        "{}",
        testbed.tatolab_stderr()
    );
    assert_eq!(
        testbed
            .tatolab_stderr()
            .matches("tatolab dev: restarting the stream")
            .count(),
        1
    );

    running_tatolab.send_signal(libc::SIGINT);
    assert_eq!(running_tatolab.wait_for_exit().code(), Some(0));
    assert_eq!(testbed.started_tatolabd_process_ids().len(), 2);
}

#[test]
fn dev_queues_exactly_one_recompile_for_an_edit_during_a_compile() {
    let testbed = AttachedTatolabdTestbed::new();
    let mut running_tatolab = testbed.start_tatolab(&testbed.project_directory(), &["dev"]);
    let first_tatolabd_process_id = testbed.wait_for_tatolabd_start_count(1)[0].process_id;

    testbed.set_control("compile_delay_seconds", "1.5");
    let edited_stream_graph = json!({"stream": "probe", "nodes": [{"name": "edited"}]});
    testbed.set_compile_document(&edited_stream_graph);
    testbed.edit_project_file("stream.py", "# stream v2\n");
    wait_until("the recompile to start", || {
        testbed.compile_invocations().len() >= 2
    });
    testbed.edit_project_file("nodes/brightness_meter.py", "# edited during the compile\n");
    testbed.wait_for_tatolab_stderr_to_contain("tatolab dev: another edit — recompiling");
    let tatolabd_runs = testbed.wait_for_tatolabd_start_count(2);

    assert_eq!(testbed.compile_invocations().len(), 3);
    assert_eq!(
        testbed.tatolabd_run(first_tatolabd_process_id).signals,
        ["INT"]
    );
    assert_eq!(tatolabd_runs[1].stream_graph, edited_stream_graph);

    running_tatolab.send_signal(libc::SIGINT);
    assert_eq!(running_tatolab.wait_for_exit().code(), Some(0));
    assert_eq!(testbed.compile_invocations().len(), 3);
    assert_eq!(testbed.started_tatolabd_process_ids().len(), 2);
}

#[test]
fn dev_sends_one_interrupt_when_the_user_interrupts_a_restart() {
    let testbed = AttachedTatolabdTestbed::new();
    let mut running_tatolab = testbed.start_tatolab(&testbed.project_directory(), &["dev"]);
    let first_tatolabd_process_id = testbed.wait_for_tatolabd_start_count(1)[0].process_id;

    testbed.set_control("tatolabd_exit_delay_seconds", "1");
    testbed.edit_project_file("stream.py", "# stream v2\n");
    testbed.wait_for_tatolab_stderr_to_contain("tatolab dev: restarting the stream");
    running_tatolab.send_signal(libc::SIGINT);

    assert_eq!(running_tatolab.wait_for_exit().code(), Some(0));
    assert_eq!(
        testbed.tatolabd_run(first_tatolabd_process_id).signals,
        ["INT"]
    );
    assert_eq!(testbed.started_tatolabd_process_ids().len(), 1);
}

#[test]
fn dev_waits_for_an_edit_when_the_first_compile_fails() {
    let testbed = AttachedTatolabdTestbed::new();
    testbed.set_control("compile_exit_code", "1");
    testbed.set_control(
        "compile_stderr",
        "error: no stream.py in the probe project\n",
    );
    let mut running_tatolab = testbed.start_tatolab(&testbed.project_directory(), &["dev"]);
    testbed.wait_for_tatolab_stderr_to_contain(
        "tatolab dev: no stream is running — fix the error and save again",
    );
    assert!(running_tatolab.is_still_running());
    assert!(testbed.started_tatolabd_process_ids().is_empty());

    testbed.clear_control("compile_exit_code");
    testbed.clear_control("compile_stderr");
    testbed.edit_project_file("stream.py", "# stream v2, fixed\n");
    let tatolabd_runs = testbed.wait_for_tatolabd_start_count(1);
    assert_eq!(testbed.compile_invocations().len(), 2);

    running_tatolab.send_signal(libc::SIGINT);
    assert_eq!(running_tatolab.wait_for_exit().code(), Some(0));
    assert_eq!(
        testbed.tatolabd_run(tatolabd_runs[0].process_id).signals,
        ["INT"]
    );
}

#[test]
fn dev_ends_with_a_usage_error_from_the_first_compile() {
    let testbed = AttachedTatolabdTestbed::new();
    testbed.set_control("compile_exit_code", "2");
    testbed.set_control("compile_stderr", "usage: the probe compile entry\n");
    let (exit_status, tatolab_stderr) = testbed.run_tatolab_to_exit(&["dev"]);
    assert_eq!(exit_status.code(), Some(2));
    assert_eq!(tatolab_stderr, "usage: the probe compile entry\n");
    assert!(testbed.started_tatolabd_process_ids().is_empty());
}

#[test]
fn dev_terminates_its_tatolabd_when_a_failed_recompile_spawn_ends_it_early() {
    let testbed = AttachedTatolabdTestbed::new();
    let mut running_tatolab = testbed.start_tatolab(&testbed.project_directory(), &["dev"]);
    let tatolabd_process_id = testbed.wait_for_tatolabd_start_count(1)[0].process_id;

    fs::remove_file(testbed.project_interpreter()).unwrap();
    testbed.edit_project_file("stream.py", "# stream v2\n");

    assert_eq!(running_tatolab.wait_for_exit().code(), Some(1));
    assert!(
        testbed.tatolab_stderr().contains(&format!(
            "error: cannot run {}",
            testbed.project_interpreter().display()
        )),
        "{}",
        testbed.tatolab_stderr()
    );
    let tatolabd_run = testbed.tatolabd_run(tatolabd_process_id);
    assert_eq!(tatolabd_run.signals, ["TERM"]);
    assert!(tatolabd_run.exit_code.is_some());
    // SAFETY: `kill` with signal 0 reads no memory and delivers nothing.
    let probe_result = unsafe { libc::kill(tatolabd_process_id as libc::pid_t, 0) };
    assert_eq!(probe_result, -1, "tatolabd outlived tatolab");
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ESRCH)
    );
}

#[test]
fn dev_ends_the_compile_in_flight_when_a_failed_restart_ends_it_early() {
    let testbed = AttachedTatolabdTestbed::new();
    let mut running_tatolab = testbed.start_tatolab(&testbed.project_directory(), &["dev"]);
    testbed.wait_for_tatolabd_start_count(1);

    testbed.set_control("tatolabd_exit_delay_seconds", "3");
    testbed.edit_project_file("stream.py", "# stream v2\n");
    testbed.wait_for_tatolab_stderr_to_contain("tatolab dev: restarting the stream");
    fs::remove_file(testbed.bin_directory().join("tatolabd")).unwrap();
    testbed.set_control("compile_delay_seconds", "30");
    testbed.edit_project_file("stream.py", "# stream v3\n");
    wait_until("the third compile to start", || {
        testbed.compile_process_ids().len() >= 3
    });
    let compile_in_flight_process_id = testbed.compile_process_ids()[2];

    assert_eq!(running_tatolab.wait_for_exit().code(), Some(1));
    assert!(
        testbed.tatolab_stderr().contains("error: cannot start "),
        "{}",
        testbed.tatolab_stderr()
    );
    // SAFETY: `kill` with signal 0 reads no memory and delivers nothing.
    let probe_result = unsafe { libc::kill(compile_in_flight_process_id as libc::pid_t, 0) };
    assert_eq!(probe_result, -1, "the compile in flight outlived tatolab");
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ESRCH)
    );
}
