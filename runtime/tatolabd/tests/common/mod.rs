// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Running the built `tatolabd` from a runtime unit of its own, with its state
//! kept out of the machine's.

#![allow(dead_code)]

use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Output, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// The file a lend must hold for `tatolabd` to take it.
const PROCESSOR_INTERPRETER_BOOTSTRAP_RELATIVE_TO_THE_LEND: &str =
    "tatolab/runtime/_processor_interpreter_bootstrap.py";

/// A temporary runtime unit: `bin/tatolabd`, and the lend beside it unless the
/// test asked for none.
pub struct TemporaryRuntimeUnit {
    _runtime_unit_root: tempfile::TempDir,
    pub tatolabd: PathBuf,
}

impl TemporaryRuntimeUnit {
    /// `bin/tatolabd` beside `lib/tatolab/lend/` holding the bootstrap.
    pub fn with_its_lend() -> Self {
        let runtime_unit = Self::without_a_lend();
        let bootstrap = runtime_unit
            .tatolabd
            .parent()
            .and_then(Path::parent)
            .expect("bin/ has a parent")
            .join("lib/tatolab/lend")
            .join(PROCESSOR_INTERPRETER_BOOTSTRAP_RELATIVE_TO_THE_LEND);
        std::fs::create_dir_all(bootstrap.parent().expect("the bootstrap has a directory"))
            .expect("the lend is created");
        std::fs::write(&bootstrap, "").expect("the bootstrap is written");
        runtime_unit
    }

    /// `bin/tatolabd` and nothing beside it.
    ///
    /// Linked, or else copied, never symlinked: `tatolabd` finds its lend from
    /// its own canonical path, which a symlink would point back at the build.
    pub fn without_a_lend() -> Self {
        let runtime_unit_root = tempfile::Builder::new()
            .prefix("tatolabd-runtime-unit-")
            .tempdir_in(env!("CARGO_TARGET_TMPDIR"))
            .expect("a temporary runtime unit");
        let bin_directory = runtime_unit_root.path().join("bin");
        std::fs::create_dir_all(&bin_directory).expect("bin/ is created");
        let tatolabd = bin_directory.join("tatolabd");
        let built_tatolabd = Path::new(env!("CARGO_BIN_EXE_tatolabd"));
        if std::fs::hard_link(built_tatolabd, &tatolabd).is_err() {
            std::fs::copy(built_tatolabd, &tatolabd).expect("tatolabd is copied into bin/");
        }
        Self {
            _runtime_unit_root: runtime_unit_root,
            tatolabd,
        }
    }
}

/// Where one `tatolabd` run keeps what it writes: its working directory, its
/// `STREAMLIB_HOME`, its `XDG_RUNTIME_DIR`, and a project directory.
///
/// Under the shared temporary directory rather than the target directory,
/// because the runtime directory's socket paths must fit `sun_path`.
pub struct TatolabdRunState {
    state_root: tempfile::TempDir,
}

impl TatolabdRunState {
    pub fn new() -> Self {
        let state_root = tempfile::Builder::new()
            .prefix("tatolabd-")
            .tempdir()
            .expect("a temporary state directory");
        for directory in ["home", "xdg", "project"] {
            std::fs::create_dir_all(state_root.path().join(directory))
                .expect("a state directory is created");
        }
        Self { state_root }
    }

    pub fn path(&self) -> &Path {
        self.state_root.path()
    }

    pub fn project_directory(&self) -> PathBuf {
        self.state_root.path().join("project")
    }

    pub fn runtime_directory(&self) -> PathBuf {
        self.state_root.path().join("xdg/streamlib")
    }

    /// Write `stream_graph` as a graph file and return its path.
    pub fn write_stream_graph(&self, stream_graph: &serde_json::Value) -> PathBuf {
        let stream_graph_file = self.state_root.path().join("stream-graph.json");
        std::fs::write(&stream_graph_file, stream_graph.to_string())
            .expect("the graph file is written");
        stream_graph_file
    }

    /// A `tatolabd` command whose state lands in this run's directories and
    /// that inherits none of the engine's settings from the test's own
    /// environment.
    pub fn tatolabd_command(&self, tatolabd: &Path) -> Command {
        let mut tatolabd_command = Command::new(tatolabd);
        tatolabd_command
            .current_dir(self.state_root.path())
            .env("STREAMLIB_HOME", self.state_root.path().join("home"))
            .env("XDG_RUNTIME_DIR", self.state_root.path().join("xdg"))
            .env_remove("STREAMLIB_RUNTIME_NAME")
            .env_remove("STREAMLIB_RUNTIME_ID")
            .env_remove("STREAMLIB_APP_DIRECTORY")
            .env_remove("STREAMLIB_QUIET")
            .env_remove("STREAMLIB_DANGEROUSLY_DEFER_LOGGING_TO_HOST")
            .stdin(Stdio::null());
        tatolabd_command
    }
}

/// An interpreter path that exists and is executable, for a run that never
/// starts a processor interpreter.
pub fn an_executable_standing_in_for_the_interpreter() -> PathBuf {
    PathBuf::from("/bin/sh")
}

/// Run `tatolabd_command` to its exit within `budget`, collecting both streams.
pub fn run_to_exit_within(mut tatolabd_command: Command, budget: Duration) -> Output {
    let child = tatolabd_command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("tatolabd starts");
    let mut spawned = SpawnedTatolabd::collecting_its_streams(child);
    let status = spawned.wait_for_exit_within(budget);
    Output {
        status,
        stdout: spawned.standard_output().into_bytes(),
        stderr: spawned.standard_error().into_bytes(),
    }
}

/// A running `tatolabd` whose standard streams are read as it writes them.
pub struct SpawnedTatolabd {
    child: Child,
    standard_error_lines: Arc<Mutex<Vec<String>>>,
    standard_output: Arc<Mutex<String>>,
    stream_readers: Vec<std::thread::JoinHandle<()>>,
}

impl SpawnedTatolabd {
    pub fn spawn(mut tatolabd_command: Command) -> Self {
        let child = tatolabd_command
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("tatolabd starts");
        Self::collecting_its_streams(child)
    }

    fn collecting_its_streams(mut child: Child) -> Self {
        let standard_error_lines = Arc::new(Mutex::new(Vec::new()));
        let standard_output = Arc::new(Mutex::new(String::new()));
        let child_standard_error = child.stderr.take().expect("stderr is piped");
        let child_standard_output = child.stdout.take().expect("stdout is piped");
        let standard_error_reader = {
            let standard_error_lines = Arc::clone(&standard_error_lines);
            std::thread::spawn(move || {
                for line in BufReader::new(child_standard_error).lines() {
                    let Ok(line) = line else { break };
                    standard_error_lines.lock().unwrap().push(line);
                }
            })
        };
        let standard_output_reader = {
            let standard_output = Arc::clone(&standard_output);
            std::thread::spawn(move || {
                let mut child_standard_output = child_standard_output;
                let mut everything_written = String::new();
                let _ = child_standard_output.read_to_string(&mut everything_written);
                standard_output
                    .lock()
                    .unwrap()
                    .push_str(&everything_written);
            })
        };
        Self {
            child,
            standard_error_lines,
            standard_output,
            stream_readers: vec![standard_error_reader, standard_output_reader],
        }
    }

    pub fn process_id(&self) -> i32 {
        self.child.id() as i32
    }

    /// Block until a line of standard error contains `marker`, or panic naming
    /// what it carried after `budget`.
    pub fn wait_until_standard_error_carries(&mut self, marker: &str, budget: Duration) {
        let deadline = Instant::now() + budget;
        loop {
            if self
                .standard_error_lines
                .lock()
                .unwrap()
                .iter()
                .any(|line| line.contains(marker))
            {
                return;
            }
            if let Ok(Some(status)) = self.child.try_wait() {
                self.join_the_stream_readers();
                panic!(
                    "tatolabd exited ({status}) before its standard error carried `{marker}`:\n{}",
                    self.standard_error()
                );
            }
            if Instant::now() >= deadline {
                let _ = self.child.kill();
                let _ = self.child.wait();
                self.join_the_stream_readers();
                panic!(
                    "tatolabd's standard error did not carry `{marker}` within {budget:?}:\n{}",
                    self.standard_error()
                );
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// Deliver `signal` to the process.
    pub fn deliver(&self, signal: i32) {
        // SAFETY: `kill` reads only its two integer arguments.
        let delivered = unsafe { libc::kill(self.process_id(), signal) };
        assert_eq!(delivered, 0, "the signal was not delivered");
    }

    /// Wait for the process to exit within `budget`, killing it and panicking
    /// if it does not; the stream readers are joined before this returns.
    pub fn wait_for_exit_within(&mut self, budget: Duration) -> ExitStatus {
        let deadline = Instant::now() + budget;
        loop {
            if let Some(status) = self.child.try_wait().expect("the child can be waited on") {
                self.join_the_stream_readers();
                return status;
            }
            if Instant::now() >= deadline {
                let _ = self.child.kill();
                let _ = self.child.wait();
                self.join_the_stream_readers();
                panic!(
                    "tatolabd did not exit within {budget:?}:\n{}",
                    self.standard_error()
                );
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fn join_the_stream_readers(&mut self) {
        for stream_reader in self.stream_readers.drain(..) {
            let _ = stream_reader.join();
        }
    }

    pub fn standard_error(&self) -> String {
        self.standard_error_lines.lock().unwrap().join("\n")
    }

    pub fn standard_output(&self) -> String {
        self.standard_output.lock().unwrap().clone()
    }
}

impl Drop for SpawnedTatolabd {
    fn drop(&mut self) {
        if let Ok(None) = self.child.try_wait() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}
