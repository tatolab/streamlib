// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::ffi::OsString;
use std::io::{Read, Write};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdout, Command, ExitStatus, Stdio};
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::Duration;

use crate::forwarded_signal_listener::block_forwarded_signals_and_listen;
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

/// Which verb launched the stream: `dev` adds the restart on edit.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum StreamLaunchVerb {
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
    Failed { compile_exit_code: u8 },
}

struct StreamCompileInFlight {
    compile_child: Child,
    compile_stdout_collector: JoinHandle<std::io::Result<Vec<u8>>>,
}

struct AttachedTatolabd {
    tatolabd_child: Child,
    // Held so the graph file outlives the `tatolabd` reading it; dropping it deletes the file.
    _stream_graph_file: tempfile::NamedTempFile,
}

fn exit_code_for(exit_status: ExitStatus) -> u8 {
    match (exit_status.code(), exit_status.signal()) {
        (Some(exit_code), _) => (exit_code & 0xff) as u8,
        (None, Some(terminating_signal)) => (128 + terminating_signal).min(255) as u8,
        (None, None) => 1,
    }
}

fn exit_code_for_signal(delivered_signal: libc::c_int) -> u8 {
    (128 + delivered_signal).min(255) as u8
}

fn described_exit(exit_status: ExitStatus) -> String {
    match (exit_status.code(), exit_status.signal()) {
        (Some(exit_code), _) => format!("exit code {exit_code}"),
        (None, Some(terminating_signal)) => format!("signal {terminating_signal}"),
        (None, None) => "an unknown status".to_owned(),
    }
}

fn send_signal_to_process(process_id: u32, forwarded_signal: libc::c_int) {
    unsafe {
        libc::kill(process_id as libc::pid_t, forwarded_signal);
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
    let probe_status = Command::new(project_venv_interpreter)
        .args(["-I", "-c", TATOLAB_STREAM_IMPORT_PROBE])
        .current_dir(project_anchor_directory)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|io_failure| {
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
    let mut compile_child = Command::new(&stream_launch_environment.project_venv_interpreter)
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
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|io_failure| {
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
        compile_stdout_collector,
    })
}

fn abandon_stream_compile(mut stream_compile_in_flight: StreamCompileInFlight) {
    let _ = stream_compile_in_flight.compile_child.kill();
    let _ = stream_compile_in_flight.compile_child.wait();
}

fn compiled_stream_from_compile_document(compile_stdout: &[u8]) -> Result<CompiledStream, String> {
    let compile_document: serde_json::Value = serde_json::from_slice(compile_stdout)
        .map_err(|parse_failure| format!("its stdout is not one JSON document: {parse_failure}"))?;
    let stream_graph = compile_document
        .get("stream_graph")
        .ok_or("its document has no `stream_graph`")?;
    let compiled_project_directory = compile_document
        .get("project_directory")
        .and_then(serde_json::Value::as_str)
        .ok_or("its document has no `project_directory` string")?;
    let mut stream_graph_file = tempfile::Builder::new()
        .prefix("tatolab-stream-graph-")
        .suffix(".json")
        .tempfile()
        .map_err(|io_failure| format!("cannot create the stream graph file: {io_failure}"))?;
    serde_json::to_writer(&mut stream_graph_file, stream_graph)
        .map_err(|write_failure| format!("cannot write the stream graph file: {write_failure}"))?;
    stream_graph_file
        .flush()
        .map_err(|io_failure| format!("cannot write the stream graph file: {io_failure}"))?;
    Ok(CompiledStream {
        stream_graph_file,
        compiled_project_directory: PathBuf::from(compiled_project_directory),
    })
}

fn finish_stream_compile(
    stream_compile_in_flight: StreamCompileInFlight,
    compile_exit_status: ExitStatus,
    stream_launch_environment: &StreamLaunchEnvironment,
) -> StreamCompileOutcome {
    let collected_compile_stdout = stream_compile_in_flight.compile_stdout_collector.join();
    if !compile_exit_status.success() {
        return StreamCompileOutcome::Failed {
            compile_exit_code: exit_code_for(compile_exit_status),
        };
    }
    let compiled_stream = match collected_compile_stdout {
        // An app that exits 0 on purpose while it compiles leaves no document: a clean stop.
        Ok(Ok(compile_stdout)) if compile_stdout.trim_ascii().is_empty() => {
            return StreamCompileOutcome::Failed {
                compile_exit_code: 0,
            };
        }
        Ok(Ok(compile_stdout)) => compiled_stream_from_compile_document(&compile_stdout),
        Ok(Err(io_failure)) => Err(format!("its stdout could not be read: {io_failure}")),
        Err(_) => Err("its stdout could not be read".to_owned()),
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

/// `tatolab run` and `tatolab dev`: compile in the project's venv, then host the stream on an
/// attached `tatolabd`, forwarding the user's signals to it; `dev` recompiles and restarts on edit.
pub fn launch_stream_on_attached_tatolabd(
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

    let mut stream_compile_in_flight = Some(start_stream_compile(
        &stream_launch_environment,
        stream_launch_verb,
    )?);
    let mut first_compile_pending = true;
    let mut recompile_requested_during_compile = false;
    let mut attached_tatolabd: Option<AttachedTatolabd> = None;
    // Some once `tatolabd` has been sent SIGINT to make way for this newer stream.
    let mut compiled_stream_awaiting_restart: Option<CompiledStream> = None;
    let mut user_stop_signal: Option<libc::c_int> = None;

    loop {
        match supervisor_event_receiver.recv_timeout(CHILD_EXIT_POLL_INTERVAL) {
            Ok(StreamLaunchSupervisorEvent::ForwardedSignalDelivered(delivered_signal)) => {
                let restart_interrupt_already_sent =
                    compiled_stream_awaiting_restart.take().is_some();
                let first_user_stop_signal = user_stop_signal.is_none();
                user_stop_signal.get_or_insert(delivered_signal);
                recompile_requested_during_compile = false;
                if let Some(abandoned_compile) = stream_compile_in_flight.take() {
                    abandon_stream_compile(abandoned_compile);
                }
                // The restart's SIGINT already began the graceful teardown; forwarding the user's
                // first SIGINT too would advance tatolabd's ladder to a forced teardown.
                let interrupt_already_delivered = restart_interrupt_already_sent
                    && first_user_stop_signal
                    && delivered_signal == libc::SIGINT;
                match &attached_tatolabd {
                    Some(_) if interrupt_already_delivered => {}
                    Some(running_tatolabd) => send_signal_to_process(
                        running_tatolabd.tatolabd_child.id(),
                        delivered_signal,
                    ),
                    None => return Ok(exit_code_for_signal(delivered_signal)),
                }
            }
            Ok(StreamLaunchSupervisorEvent::ProjectSourcesChanged) => {
                if user_stop_signal.is_none() {
                    if stream_compile_in_flight.is_some() {
                        recompile_requested_during_compile = true;
                    } else {
                        eprintln!("tatolab dev: an edit — recompiling");
                        stream_compile_in_flight = Some(start_stream_compile(
                            &stream_launch_environment,
                            stream_launch_verb,
                        )?);
                    }
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) | Err(mpsc::RecvTimeoutError::Disconnected) => {}
        }

        let finished_compile_exit_status = match stream_compile_in_flight.as_mut() {
            Some(running_compile) => {
                running_compile
                    .compile_child
                    .try_wait()
                    .map_err(|io_failure| {
                        TatolabCommandFailure::refused(format!(
                            "cannot wait for the compile: {io_failure}"
                        ))
                    })?
            }
            None => None,
        };
        if let Some(compile_exit_status) = finished_compile_exit_status
            && let Some(finished_compile) = stream_compile_in_flight.take()
        {
            let stream_compile_outcome = finish_stream_compile(
                finished_compile,
                compile_exit_status,
                &stream_launch_environment,
            );
            let this_was_the_first_compile = std::mem::take(&mut first_compile_pending);
            if recompile_requested_during_compile {
                recompile_requested_during_compile = false;
                eprintln!("tatolab dev: another edit — recompiling");
                stream_compile_in_flight = Some(start_stream_compile(
                    &stream_launch_environment,
                    stream_launch_verb,
                )?);
                first_compile_pending = this_was_the_first_compile;
            } else {
                match stream_compile_outcome {
                    StreamCompileOutcome::Compiled(compiled_stream) => match &attached_tatolabd {
                        Some(running_tatolabd) => {
                            if compiled_stream_awaiting_restart.is_none() {
                                eprintln!("tatolab dev: restarting the stream");
                                send_signal_to_process(
                                    running_tatolabd.tatolabd_child.id(),
                                    libc::SIGINT,
                                );
                            }
                            compiled_stream_awaiting_restart = Some(compiled_stream);
                        }
                        None => {
                            attached_tatolabd = Some(start_attached_tatolabd(
                                &stream_launch_environment,
                                compiled_stream,
                            )?);
                        }
                    },
                    StreamCompileOutcome::Failed { compile_exit_code } => {
                        // Exit 2 on the first compile is a usage error in the flags, which no
                        // edit can fix.
                        if stream_launch_verb == StreamLaunchVerb::Run
                            || (this_was_the_first_compile
                                && compile_exit_code == COMPILE_ENTRY_USAGE_ERROR_EXIT_CODE)
                        {
                            return Err(TatolabCommandFailure::already_reported(compile_exit_code));
                        }
                        if attached_tatolabd.is_some() {
                            eprintln!(
                                "tatolab dev: kept the running stream — fix the error and save again"
                            );
                        } else {
                            eprintln!(
                                "tatolab dev: no stream is running — fix the error and save again"
                            );
                        }
                    }
                }
            }
        }

        let tatolabd_exit_status = match attached_tatolabd.as_mut() {
            Some(running_tatolabd) => {
                running_tatolabd
                    .tatolabd_child
                    .try_wait()
                    .map_err(|io_failure| {
                        TatolabCommandFailure::refused(format!(
                            "cannot wait for tatolabd: {io_failure}"
                        ))
                    })?
            }
            None => None,
        };
        if let Some(tatolabd_exit_status) = tatolabd_exit_status {
            attached_tatolabd = None;
            if user_stop_signal.is_some() || stream_launch_verb == StreamLaunchVerb::Run {
                return Ok(exit_code_for(tatolabd_exit_status));
            }
            match compiled_stream_awaiting_restart.take() {
                Some(compiled_stream) => {
                    if !tatolabd_exit_status.success() {
                        eprintln!(
                            "tatolab dev: the previous stream exited with {}",
                            described_exit(tatolabd_exit_status)
                        );
                    }
                    attached_tatolabd = Some(start_attached_tatolabd(
                        &stream_launch_environment,
                        compiled_stream,
                    )?);
                }
                None => eprintln!(
                    "tatolab dev: tatolabd exited with {} — waiting for the next edit",
                    described_exit(tatolabd_exit_status)
                ),
            }
        }
    }
}
