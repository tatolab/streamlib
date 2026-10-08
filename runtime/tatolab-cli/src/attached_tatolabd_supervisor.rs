// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::ffi::OsString;
use std::io::{Read, Write};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdout, Command, ExitStatus, Stdio};
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::forwarded_signal_listener::{
    block_forwarded_signals_and_listen, unblock_forwarded_signals_in_the_child,
};
use crate::project_source_change_watcher::watch_project_sources;
use crate::{StreamLaunchArguments, TatolabCommandFailure};

/// The module `tatolab.stream` compiles a project's stream with, run in the project's venv.
const PROJECT_STREAM_COMPILE_ENTRY_MODULE: &str = "tatolab.stream._project_stream_compile_entry";

const TATOLAB_STREAM_IMPORT_PROBE: &str = "import importlib.util, sys; \
     sys.exit(0 if importlib.util.find_spec('tatolab.stream') is not None else 1)";

/// The engine variable naming the directory a built-in keys its machine-visible names on.
const APP_DIRECTORY_ENVIRONMENT_VARIABLE: &str = "STREAMLIB_APP_DIRECTORY";

/// The engine variable naming the runtime.
const RUNTIME_NAME_ENVIRONMENT_VARIABLE: &str = "STREAMLIB_RUNTIME_NAME";

/// The compile entry's exit code for an argparse usage error.
const COMPILE_ENTRY_USAGE_ERROR_EXIT_CODE: u8 = 2;

/// How long the supervisor waits for an event before polling its children's exits.
const CHILD_EXIT_POLL_INTERVAL: Duration = Duration::from_millis(20);

/// How long a `tatolabd` left running by an early exit has to stop on SIGTERM before it is killed.
///
/// Must exceed the engine's teardown watchdog (15 s) plus its log-flush grace, so `tatolab` kills
/// only a `tatolabd` whose own watchdog failed to end it. A literal: `tatolab` links no engine.
const ABANDONED_TATOLABD_TERMINATION_GRACE: Duration = Duration::from_secs(20);

/// Which verb launched the stream: `dev` adds the restart on edit.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum StreamLaunchVerb {
    /// `tatolab run`.
    Run,
    /// `tatolab dev`.
    Dev,
}

impl StreamLaunchVerb {
    fn compile_entry_verb(self) -> &'static str {
        match self {
            StreamLaunchVerb::Run => "run",
            StreamLaunchVerb::Dev => "dev",
        }
    }
}

enum StreamLaunchSupervisorEvent {
    ForwardedSignalDelivered(libc::c_int),
    ProjectSourcesChanged,
}

struct StreamLaunchEnvironment {
    project_anchor_directory: PathBuf,
    project_venv_interpreter: PathBuf,
    tatolabd_executable: PathBuf,
    compile_entry_arguments: Vec<OsString>,
    requested_runtime_name: Option<OsString>,
}

struct CompiledStream {
    stream_graph_file: tempfile::NamedTempFile,
    compiled_project_directory: PathBuf,
}

enum StreamCompileOutcome {
    Compiled(CompiledStream),
    /// The app exited 0 on purpose while it compiled, leaving no document.
    EndedWithoutAStream,
    Failed {
        compile_exit_code: u8,
    },
}

struct StreamCompileInFlight {
    compile_child: Child,
    // Taken by `finish_stream_compile` once the compile has exited.
    compile_stdout_collector: Option<JoinHandle<std::io::Result<Vec<u8>>>>,
}

// An early return must never leave a compile running unreaped. The collector is left to end on
// its own rather than joined: a process the compile started can hold its stdout open past the
// compile's death, and joining would block `tatolab` on it.
impl Drop for StreamCompileInFlight {
    fn drop(&mut self) {
        let _ = self.compile_child.kill();
        let _ = self.compile_child.wait();
    }
}

struct AttachedTatolabd {
    tatolabd_child: Child,
    // Held so the graph file outlives the `tatolabd` reading it; dropping it deletes the file.
    _stream_graph_file: tempfile::NamedTempFile,
}

// An early return must never leave `tatolabd` holding the GPU and camera: macOS has no
// parent-death signal to end it.
impl Drop for AttachedTatolabd {
    fn drop(&mut self) {
        if !matches!(self.tatolabd_child.try_wait(), Ok(None)) {
            return;
        }
        send_signal_to_process(self.tatolabd_child.id(), libc::SIGTERM);
        let termination_requested_at = Instant::now();
        while termination_requested_at.elapsed() < ABANDONED_TATOLABD_TERMINATION_GRACE {
            if !matches!(self.tatolabd_child.try_wait(), Ok(None)) {
                return;
            }
            std::thread::sleep(CHILD_EXIT_POLL_INTERVAL);
        }
        eprintln!(
            "tatolab: tatolabd did not stop within {} s of SIGTERM — killing it",
            ABANDONED_TATOLABD_TERMINATION_GRACE.as_secs()
        );
        let _ = self.tatolabd_child.kill();
        let _ = self.tatolabd_child.wait();
    }
}

/// How a child process ended, read once from its [`ExitStatus`].
enum ChildProcessEnding {
    ExitedWithCode(i32),
    KilledBySignal(libc::c_int),
    Unknown,
}

impl ChildProcessEnding {
    fn of(exit_status: ExitStatus) -> Self {
        match (exit_status.code(), exit_status.signal()) {
            (Some(exit_code), _) => ChildProcessEnding::ExitedWithCode(exit_code),
            (None, Some(terminating_signal)) => {
                ChildProcessEnding::KilledBySignal(terminating_signal)
            }
            (None, None) => ChildProcessEnding::Unknown,
        }
    }
}

fn exit_code_for(exit_status: ExitStatus) -> u8 {
    match ChildProcessEnding::of(exit_status) {
        ChildProcessEnding::ExitedWithCode(exit_code) => (exit_code & 0xff) as u8,
        ChildProcessEnding::KilledBySignal(terminating_signal) => {
            exit_code_for_signal(terminating_signal)
        }
        ChildProcessEnding::Unknown => 1,
    }
}

fn exit_code_for_signal(delivered_signal: libc::c_int) -> u8 {
    (128 + delivered_signal).min(255) as u8
}

fn described_exit(exit_status: ExitStatus) -> String {
    match ChildProcessEnding::of(exit_status) {
        ChildProcessEnding::ExitedWithCode(exit_code) => format!("exit code {exit_code}"),
        ChildProcessEnding::KilledBySignal(terminating_signal) => {
            format!("signal {terminating_signal}")
        }
        ChildProcessEnding::Unknown => "an unknown status".to_owned(),
    }
}

fn send_signal_to_process(process_id: u32, sent_signal: libc::c_int) {
    // SAFETY: `kill` reads no memory; the pid is a child this process has not yet reaped.
    if unsafe { libc::kill(process_id as libc::pid_t, sent_signal) } == 0 {
        return;
    }
    let kill_failure = std::io::Error::last_os_error();
    // ESRCH: the process is already gone, so nothing is left for the signal to stop.
    if kill_failure.raw_os_error() != Some(libc::ESRCH) {
        eprintln!(
            "tatolab: cannot send signal {sent_signal} to process {process_id}: {kill_failure}"
        );
    }
}

fn resolve_project_anchor_directory(
    requested_anchor_directory: Option<&OsString>,
) -> Result<PathBuf, TatolabCommandFailure> {
    let caller_working_directory = std::env::current_dir().map_err(|io_failure| {
        TatolabCommandFailure::refused(format!("cannot read the working directory: {io_failure}"))
    })?;
    let Some(requested_anchor_directory) = requested_anchor_directory else {
        return Ok(caller_working_directory);
    };
    caller_working_directory
        .join(requested_anchor_directory)
        .canonicalize()
        .ok()
        .filter(|canonical_anchor_directory| canonical_anchor_directory.is_dir())
        .ok_or_else(|| {
            TatolabCommandFailure::refused(format!(
                "--dir {} is not a directory",
                Path::new(requested_anchor_directory).display()
            ))
        })
}

fn locate_tatolabd_beside_this_executable() -> Result<PathBuf, TatolabCommandFailure> {
    let this_executable = std::env::current_exe()
        .and_then(|current_executable| current_executable.canonicalize())
        .map_err(|io_failure| {
            TatolabCommandFailure::refused(format!(
                "cannot locate the tatolab executable: {io_failure}"
            ))
        })?;
    let executable_directory = this_executable
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_default();
    let tatolabd_executable = executable_directory.join("tatolabd");
    if !tatolabd_executable.is_file() {
        return Err(TatolabCommandFailure::refused(format!(
            "no tatolabd beside {} — build the runtime unit with `cargo xtask build-runtime`",
            this_executable.display()
        )));
    }
    Ok(tatolabd_executable)
}

fn refuse_a_venv_without_tatolab_stream(
    project_anchor_directory: &Path,
    project_venv_interpreter: &Path,
) -> Result<(), TatolabCommandFailure> {
    let mut probe_command = Command::new(project_venv_interpreter);
    probe_command
        .args(["-I", "-c", TATOLAB_STREAM_IMPORT_PROBE])
        .current_dir(project_anchor_directory)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    unblock_forwarded_signals_in_the_child(&mut probe_command);
    let probe_status = probe_command.status().map_err(|io_failure| {
        TatolabCommandFailure::refused(format!(
            "cannot run {}: {io_failure}",
            project_venv_interpreter.display()
        ))
    })?;
    if !probe_status.success() {
        return Err(TatolabCommandFailure::refused(format!(
            "the virtual environment at {} cannot import tatolab.stream — add `tatolab-stream` \
             to the project's dependencies and run `uv sync` in {}",
            project_anchor_directory.join(".venv").display(),
            project_anchor_directory.display()
        )));
    }
    Ok(())
}

fn resolve_stream_launch_environment(
    stream_launch_arguments: &StreamLaunchArguments,
) -> Result<StreamLaunchEnvironment, TatolabCommandFailure> {
    let project_anchor_directory = resolve_project_anchor_directory(
        stream_launch_arguments.requested_anchor_directory.as_ref(),
    )?;
    let project_venv_directory = project_anchor_directory.join(".venv");
    let project_venv_interpreter = project_venv_directory.join("bin").join("python");
    if !project_venv_interpreter.is_file() {
        return Err(TatolabCommandFailure::refused(format!(
            "no virtual environment at {} — run `uv sync` in {} first",
            project_venv_directory.display(),
            project_anchor_directory.display()
        )));
    }
    let tatolabd_executable = locate_tatolabd_beside_this_executable()?;
    refuse_a_venv_without_tatolab_stream(&project_anchor_directory, &project_venv_interpreter)?;

    let mut compile_entry_arguments = Vec::new();
    if let Some(requested_stream_target) = &stream_launch_arguments.requested_stream_target {
        compile_entry_arguments.push(requested_stream_target.clone());
    }
    for (compile_entry_flag, flag_value) in [
        ("-f", &stream_launch_arguments.requested_entry_file),
        ("--dir", &stream_launch_arguments.requested_anchor_directory),
        ("--name", &stream_launch_arguments.requested_stream_name),
    ] {
        if let Some(flag_value) = flag_value {
            compile_entry_arguments.push(OsString::from(compile_entry_flag));
            compile_entry_arguments.push(flag_value.clone());
        }
    }

    Ok(StreamLaunchEnvironment {
        project_anchor_directory,
        project_venv_interpreter,
        tatolabd_executable,
        compile_entry_arguments,
        requested_runtime_name: stream_launch_arguments.requested_runtime_name.clone(),
    })
}

fn start_stream_compile(
    stream_launch_environment: &StreamLaunchEnvironment,
    stream_launch_verb: StreamLaunchVerb,
) -> Result<StreamCompileInFlight, TatolabCommandFailure> {
    let mut compile_command = Command::new(&stream_launch_environment.project_venv_interpreter);
    compile_command
        // `-I` keeps the anchor off `sys.path` until the compile entry has imported what it
        // needs, so a project module named like a stdlib one cannot replace it.
        .args([
            "-I",
            "-m",
            PROJECT_STREAM_COMPILE_ENTRY_MODULE,
            "--verb",
            stream_launch_verb.compile_entry_verb(),
        ])
        .args(&stream_launch_environment.compile_entry_arguments)
        .current_dir(&stream_launch_environment.project_anchor_directory)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    unblock_forwarded_signals_in_the_child(&mut compile_command);
    let mut compile_child = compile_command.spawn().map_err(|io_failure| {
        TatolabCommandFailure::refused(format!(
            "cannot run {}: {io_failure}",
            stream_launch_environment.project_venv_interpreter.display()
        ))
    })?;
    let compile_stdout: Option<ChildStdout> = compile_child.stdout.take();
    let compile_stdout_collector = std::thread::Builder::new()
        .name("tatolab-compile-stdout-collector".to_owned())
        .spawn(move || {
            let mut collected_compile_stdout = Vec::new();
            if let Some(mut compile_stdout) = compile_stdout {
                compile_stdout.read_to_end(&mut collected_compile_stdout)?;
            }
            Ok(collected_compile_stdout)
        })
        .map_err(|io_failure| {
            TatolabCommandFailure::refused(format!(
                "cannot read the compile's output: {io_failure}"
            ))
        })?;
    Ok(StreamCompileInFlight {
        compile_child,
        compile_stdout_collector: Some(compile_stdout_collector),
    })
}

/// The document the compile entry prints on stdout.
#[derive(serde::Deserialize)]
struct ProjectStreamCompileDocument {
    // Raw, so the graph reaches `tatolabd` byte for byte: a round trip through `Value` would turn
    // an integer wider than 64 bits into a float and could move a float by an ULP.
    stream_graph: Box<serde_json::value::RawValue>,
    project_directory: PathBuf,
}

fn compiled_stream_from_compile_document(compile_stdout: &[u8]) -> Result<CompiledStream, String> {
    let compile_document: ProjectStreamCompileDocument = serde_json::from_slice(compile_stdout)
        .map_err(|parse_failure| {
            format!("its stdout is not the compile document: {parse_failure}")
        })?;
    let mut stream_graph_file = tempfile::Builder::new()
        .prefix("tatolab-stream-graph-")
        .suffix(".json")
        .tempfile()
        .map_err(|io_failure| format!("cannot create the stream graph file: {io_failure}"))?;
    stream_graph_file
        .write_all(compile_document.stream_graph.get().as_bytes())
        .and_then(|()| stream_graph_file.flush())
        .map_err(|io_failure| format!("cannot write the stream graph file: {io_failure}"))?;
    Ok(CompiledStream {
        stream_graph_file,
        compiled_project_directory: compile_document.project_directory,
    })
}

fn finish_stream_compile(
    mut stream_compile_in_flight: StreamCompileInFlight,
    compile_exit_status: ExitStatus,
    stream_launch_environment: &StreamLaunchEnvironment,
) -> StreamCompileOutcome {
    // Checked before the collector is joined: a failed compile's stdout is never read, and a
    // process it started may still hold the pipe open.
    if !compile_exit_status.success() {
        return StreamCompileOutcome::Failed {
            compile_exit_code: exit_code_for(compile_exit_status),
        };
    }
    let collected_compile_stdout = stream_compile_in_flight
        .compile_stdout_collector
        .take()
        .map(JoinHandle::join);
    let compiled_stream = match collected_compile_stdout {
        Some(Ok(Ok(compile_stdout))) if compile_stdout.trim_ascii().is_empty() => {
            return StreamCompileOutcome::EndedWithoutAStream;
        }
        Some(Ok(Ok(compile_stdout))) => compiled_stream_from_compile_document(&compile_stdout),
        Some(Ok(Err(io_failure))) => Err(format!("its stdout could not be read: {io_failure}")),
        Some(Err(_)) | None => Err("its stdout could not be read".to_owned()),
    };
    match compiled_stream {
        Ok(compiled_stream) => StreamCompileOutcome::Compiled(compiled_stream),
        Err(compile_document_failure) => {
            eprintln!(
                "error: the compile in {} gave no stream graph: {compile_document_failure}",
                stream_launch_environment.project_venv_interpreter.display()
            );
            StreamCompileOutcome::Failed {
                compile_exit_code: 1,
            }
        }
    }
}

fn start_attached_tatolabd(
    stream_launch_environment: &StreamLaunchEnvironment,
    compiled_stream: CompiledStream,
) -> Result<AttachedTatolabd, TatolabCommandFailure> {
    let mut tatolabd_command = Command::new(&stream_launch_environment.tatolabd_executable);
    tatolabd_command
        .arg("--stream-graph")
        .arg(compiled_stream.stream_graph_file.path())
        .arg("--project")
        .arg(&compiled_stream.compiled_project_directory)
        .arg("--interpreter")
        .arg(&stream_launch_environment.project_venv_interpreter)
        .current_dir(&stream_launch_environment.project_anchor_directory)
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .env(
            APP_DIRECTORY_ENVIRONMENT_VARIABLE,
            &stream_launch_environment.project_anchor_directory,
        )
        // Its own group: a terminal's Ctrl-C reaches `tatolab` alone, which forwards it once.
        .process_group(0);
    if let Some(requested_runtime_name) = &stream_launch_environment.requested_runtime_name {
        tatolabd_command.env(RUNTIME_NAME_ENVIRONMENT_VARIABLE, requested_runtime_name);
    }
    unblock_forwarded_signals_in_the_child(&mut tatolabd_command);
    end_tatolabd_when_tatolab_dies(&mut tatolabd_command);
    let tatolabd_child = tatolabd_command.spawn().map_err(|io_failure| {
        TatolabCommandFailure::refused(format!(
            "cannot start {}: {io_failure}",
            stream_launch_environment.tatolabd_executable.display()
        ))
    })?;
    Ok(AttachedTatolabd {
        tatolabd_child,
        _stream_graph_file: compiled_stream.stream_graph_file,
    })
}

// PR_SET_PDEATHSIG fires when the spawning *thread* exits, so `tatolabd` is spawned from the
// main thread only.
#[cfg(target_os = "linux")]
fn end_tatolabd_when_tatolab_dies(tatolabd_command: &mut Command) {
    let tatolab_process_id = std::process::id() as libc::pid_t;
    // SAFETY: the closure only calls `prctl` and `getppid`, both async-signal-safe after fork.
    unsafe {
        tatolabd_command.pre_exec(move || {
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            if libc::getppid() != tatolab_process_id {
                return Err(std::io::Error::other(
                    "tatolab exited before tatolabd started",
                ));
            }
            Ok(())
        });
    }
}

#[cfg(not(target_os = "linux"))]
fn end_tatolabd_when_tatolab_dies(_tatolabd_command: &mut Command) {}

/// What the supervisor does after handling one event.
enum AttachedStreamSupervisorNextStep {
    KeepSupervising,
    ExitTatolabWith(u8),
}

/// The state `run` and `dev` supervise one stream's compiles and its attached `tatolabd` with.
struct AttachedStreamSupervisor {
    stream_launch_verb: StreamLaunchVerb,
    stream_launch_environment: StreamLaunchEnvironment,
    stream_compile_in_flight: Option<StreamCompileInFlight>,
    first_compile_pending: bool,
    recompile_requested_during_compile: bool,
    attached_tatolabd: Option<AttachedTatolabd>,
    // Some once `tatolabd` has been sent SIGINT to make way for this newer stream.
    compiled_stream_awaiting_restart: Option<CompiledStream>,
    user_stop_signal: Option<libc::c_int>,
}

impl AttachedStreamSupervisor {
    fn start_first_compile(
        stream_launch_verb: StreamLaunchVerb,
        stream_launch_environment: StreamLaunchEnvironment,
    ) -> Result<Self, TatolabCommandFailure> {
        let first_stream_compile =
            start_stream_compile(&stream_launch_environment, stream_launch_verb)?;
        Ok(Self {
            stream_launch_verb,
            stream_launch_environment,
            stream_compile_in_flight: Some(first_stream_compile),
            first_compile_pending: true,
            recompile_requested_during_compile: false,
            attached_tatolabd: None,
            compiled_stream_awaiting_restart: None,
            user_stop_signal: None,
        })
    }

    fn start_next_compile(&mut self) -> Result<(), TatolabCommandFailure> {
        self.stream_compile_in_flight = Some(start_stream_compile(
            &self.stream_launch_environment,
            self.stream_launch_verb,
        )?);
        Ok(())
    }

    fn start_attached_tatolabd_on(
        &mut self,
        compiled_stream: CompiledStream,
    ) -> Result<(), TatolabCommandFailure> {
        self.attached_tatolabd = Some(start_attached_tatolabd(
            &self.stream_launch_environment,
            compiled_stream,
        )?);
        Ok(())
    }

    fn on_forwarded_signal(
        &mut self,
        delivered_signal: libc::c_int,
    ) -> AttachedStreamSupervisorNextStep {
        let restart_interrupt_already_sent = self.compiled_stream_awaiting_restart.take().is_some();
        let first_user_stop_signal = self.user_stop_signal.is_none();
        self.user_stop_signal.get_or_insert(delivered_signal);
        self.recompile_requested_during_compile = false;
        self.stream_compile_in_flight = None;
        // The restart's SIGINT already began the graceful teardown; forwarding the user's first
        // SIGINT too would advance tatolabd's ladder to a forced teardown.
        let interrupt_already_delivered = restart_interrupt_already_sent
            && first_user_stop_signal
            && delivered_signal == libc::SIGINT;
        match &self.attached_tatolabd {
            Some(_) if interrupt_already_delivered => {}
            Some(running_tatolabd) => {
                send_signal_to_process(running_tatolabd.tatolabd_child.id(), delivered_signal)
            }
            None => {
                return AttachedStreamSupervisorNextStep::ExitTatolabWith(exit_code_for_signal(
                    delivered_signal,
                ));
            }
        }
        AttachedStreamSupervisorNextStep::KeepSupervising
    }

    fn on_project_sources_changed(
        &mut self,
    ) -> Result<AttachedStreamSupervisorNextStep, TatolabCommandFailure> {
        if self.user_stop_signal.is_some() {
            return Ok(AttachedStreamSupervisorNextStep::KeepSupervising);
        }
        if self.stream_compile_in_flight.is_some() {
            self.recompile_requested_during_compile = true;
        } else {
            eprintln!("tatolab dev: an edit — recompiling");
            self.start_next_compile()?;
        }
        Ok(AttachedStreamSupervisorNextStep::KeepSupervising)
    }

    fn take_exited_compile(
        &mut self,
    ) -> Result<Option<(StreamCompileInFlight, ExitStatus)>, TatolabCommandFailure> {
        let Some(running_compile) = self.stream_compile_in_flight.as_mut() else {
            return Ok(None);
        };
        let compile_exit_status =
            running_compile
                .compile_child
                .try_wait()
                .map_err(|io_failure| {
                    TatolabCommandFailure::refused(format!(
                        "cannot wait for the compile: {io_failure}"
                    ))
                })?;
        Ok(compile_exit_status.and_then(|compile_exit_status| {
            self.stream_compile_in_flight
                .take()
                .map(|finished_compile| (finished_compile, compile_exit_status))
        }))
    }

    fn on_compile_exited(
        &mut self,
        finished_compile: StreamCompileInFlight,
        compile_exit_status: ExitStatus,
    ) -> Result<AttachedStreamSupervisorNextStep, TatolabCommandFailure> {
        let stream_compile_outcome = finish_stream_compile(
            finished_compile,
            compile_exit_status,
            &self.stream_launch_environment,
        );
        let this_was_the_first_compile = std::mem::take(&mut self.first_compile_pending);
        if self.recompile_requested_during_compile {
            self.recompile_requested_during_compile = false;
            eprintln!("tatolab dev: another edit — recompiling");
            self.start_next_compile()?;
            self.first_compile_pending = this_was_the_first_compile;
            return Ok(AttachedStreamSupervisorNextStep::KeepSupervising);
        }
        match stream_compile_outcome {
            StreamCompileOutcome::Compiled(compiled_stream) => match &self.attached_tatolabd {
                Some(running_tatolabd) => {
                    if self.compiled_stream_awaiting_restart.is_none() {
                        eprintln!("tatolab dev: restarting the stream");
                        send_signal_to_process(running_tatolabd.tatolabd_child.id(), libc::SIGINT);
                    }
                    self.compiled_stream_awaiting_restart = Some(compiled_stream);
                }
                None => self.start_attached_tatolabd_on(compiled_stream)?,
            },
            StreamCompileOutcome::EndedWithoutAStream => {
                if self.stream_launch_verb == StreamLaunchVerb::Run {
                    return Ok(AttachedStreamSupervisorNextStep::ExitTatolabWith(0));
                }
                if self.attached_tatolabd.is_some() {
                    eprintln!(
                        "tatolab dev: the compile ended without a stream — kept the running stream"
                    );
                } else {
                    eprintln!(
                        "tatolab dev: the compile ended without a stream — waiting for the next edit"
                    );
                }
            }
            StreamCompileOutcome::Failed { compile_exit_code } => {
                // Exit 2 on the first compile is a usage error in the flags, which no edit can fix.
                if self.stream_launch_verb == StreamLaunchVerb::Run
                    || (this_was_the_first_compile
                        && compile_exit_code == COMPILE_ENTRY_USAGE_ERROR_EXIT_CODE)
                {
                    return Err(TatolabCommandFailure::already_reported(compile_exit_code));
                }
                if self.attached_tatolabd.is_some() {
                    eprintln!(
                        "tatolab dev: kept the running stream — fix the error and save again"
                    );
                } else {
                    eprintln!("tatolab dev: no stream is running — fix the error and save again");
                }
            }
        }
        Ok(AttachedStreamSupervisorNextStep::KeepSupervising)
    }

    fn take_exited_tatolabd(&mut self) -> Result<Option<ExitStatus>, TatolabCommandFailure> {
        let Some(running_tatolabd) = self.attached_tatolabd.as_mut() else {
            return Ok(None);
        };
        let tatolabd_exit_status =
            running_tatolabd
                .tatolabd_child
                .try_wait()
                .map_err(|io_failure| {
                    TatolabCommandFailure::refused(format!(
                        "cannot wait for tatolabd: {io_failure}"
                    ))
                })?;
        if tatolabd_exit_status.is_some() {
            self.attached_tatolabd = None;
        }
        Ok(tatolabd_exit_status)
    }

    fn on_tatolabd_exited(
        &mut self,
        tatolabd_exit_status: ExitStatus,
    ) -> Result<AttachedStreamSupervisorNextStep, TatolabCommandFailure> {
        if self.user_stop_signal.is_some() || self.stream_launch_verb == StreamLaunchVerb::Run {
            return Ok(AttachedStreamSupervisorNextStep::ExitTatolabWith(
                exit_code_for(tatolabd_exit_status),
            ));
        }
        match self.compiled_stream_awaiting_restart.take() {
            Some(compiled_stream) => {
                if !tatolabd_exit_status.success() {
                    eprintln!(
                        "tatolab dev: the previous stream exited with {}",
                        described_exit(tatolabd_exit_status)
                    );
                }
                self.start_attached_tatolabd_on(compiled_stream)?;
            }
            None => eprintln!(
                "tatolab dev: tatolabd exited with {} — waiting for the next edit",
                described_exit(tatolabd_exit_status)
            ),
        }
        Ok(AttachedStreamSupervisorNextStep::KeepSupervising)
    }

    fn supervise_until_exit(
        mut self,
        supervisor_event_receiver: mpsc::Receiver<StreamLaunchSupervisorEvent>,
    ) -> Result<u8, TatolabCommandFailure> {
        loop {
            let event_next_step =
                match supervisor_event_receiver.recv_timeout(CHILD_EXIT_POLL_INTERVAL) {
                    Ok(StreamLaunchSupervisorEvent::ForwardedSignalDelivered(delivered_signal)) => {
                        self.on_forwarded_signal(delivered_signal)
                    }
                    Ok(StreamLaunchSupervisorEvent::ProjectSourcesChanged) => {
                        self.on_project_sources_changed()?
                    }
                    Err(mpsc::RecvTimeoutError::Timeout)
                    | Err(mpsc::RecvTimeoutError::Disconnected) => {
                        AttachedStreamSupervisorNextStep::KeepSupervising
                    }
                };
            if let AttachedStreamSupervisorNextStep::ExitTatolabWith(exit_code) = event_next_step {
                return Ok(exit_code);
            }
            if let Some((finished_compile, compile_exit_status)) = self.take_exited_compile()?
                && let AttachedStreamSupervisorNextStep::ExitTatolabWith(exit_code) =
                    self.on_compile_exited(finished_compile, compile_exit_status)?
            {
                return Ok(exit_code);
            }
            if let Some(tatolabd_exit_status) = self.take_exited_tatolabd()?
                && let AttachedStreamSupervisorNextStep::ExitTatolabWith(exit_code) =
                    self.on_tatolabd_exited(tatolabd_exit_status)?
            {
                return Ok(exit_code);
            }
        }
    }
}

/// `tatolab run` and `tatolab dev`: compile in the project's venv, then host the stream on an
/// attached `tatolabd`, forwarding the user's signals to it; `dev` recompiles and restarts on edit.
pub(crate) fn launch_stream_on_attached_tatolabd(
    stream_launch_verb: StreamLaunchVerb,
    stream_launch_arguments: &StreamLaunchArguments,
) -> Result<u8, TatolabCommandFailure> {
    let (supervisor_event_sender, supervisor_event_receiver) =
        mpsc::channel::<StreamLaunchSupervisorEvent>();
    let signal_event_sender = supervisor_event_sender.clone();
    block_forwarded_signals_and_listen(move |delivered_signal| {
        let _ = signal_event_sender.send(StreamLaunchSupervisorEvent::ForwardedSignalDelivered(
            delivered_signal,
        ));
    })
    .map_err(|io_failure| {
        TatolabCommandFailure::refused(format!("cannot listen for signals: {io_failure}"))
    })?;

    let stream_launch_environment = resolve_stream_launch_environment(stream_launch_arguments)?;
    if stream_launch_verb == StreamLaunchVerb::Dev {
        let source_change_event_sender = supervisor_event_sender.clone();
        watch_project_sources(
            stream_launch_environment.project_anchor_directory.clone(),
            move || {
                source_change_event_sender
                    .send(StreamLaunchSupervisorEvent::ProjectSourcesChanged)
                    .is_ok()
            },
        )
        .map_err(|io_failure| {
            TatolabCommandFailure::refused(format!("cannot watch the project: {io_failure}"))
        })?;
    }

    AttachedStreamSupervisor::start_first_compile(stream_launch_verb, stream_launch_environment)?
        .supervise_until_exit(supervisor_event_receiver)
}
