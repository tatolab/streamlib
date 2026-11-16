// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

mod dyn_generated_node_for_spawn_host;
mod helper_process_command_exchange;
mod helper_process_shutdown;
mod helper_process_spawn;

use std::collections::VecDeque;
use std::ffi::OsString;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::node_interpreter_shutdown_ladder::{
    LifecycleReplyAwaited, kill_the_process_group_and_reap_its_leader,
    wait_for_a_child_to_become_collectable_within,
};
use super::subprocess_bridge::{
    ENGINE_BUILD_ID, ENGINE_BUILD_ID_ENVIRONMENT_VARIABLE, HelperProcessShutdownCommand,
    SubprocessBridge, spawn_fd_line_reader,
};
use super::subprocess_escalate::WhatAHelperPlacedNodeReaches;
use crate::core::descriptors::NodeDescriptor;
use crate::core::error::{Error, Result};
use crate::core::execution::ExecutionConfig;
use crate::core::graph::{
    FunctionNodePromotedToItsOwnProcessComponent, GraphNode, GraphNodeWithComponents,
};
use crate::core::nodes::{FunctionNodeCallUnderWayInTheFarSide, OutOfProcessLinkWiringEnvelope};
use crate::core::runtime::{
    LoadedStreamTag, ShutdownEscalationOfOneStream, StreamEnvironment,
    start_the_interpreter_at_the_open_descriptor_soft_limit_the_runtime_started_with,
};

pub const NODE_INTERPRETER_BOOTSTRAP_PATH_IN_THE_LEND_DIRECTORY: &str =
    "tatolab/runtime/_node_interpreter_bootstrap.py";

pub(crate) const NODE_INTERPRETER_ENTRYPOINT_ENVIRONMENT_VARIABLE: &str = "STREAMLIB_ENTRYPOINT";

pub const NODE_INTERPRETER_NODE_RUN_ID_ENVIRONMENT_VARIABLE: &str = "STREAMLIB_NODE_RUN_ID";

pub const NODE_INTERPRETER_NODE_NAME_ENVIRONMENT_VARIABLE: &str = "STREAMLIB_NODE_NAME";

/// The environment variable carrying the id of the stream the hosted node
/// belongs to.
pub const NODE_INTERPRETER_STREAM_ID_ENVIRONMENT_VARIABLE: &str = "STREAMLIB_STREAM_ID";

/// The environment variable carrying the project directory of the stream the
/// hosted node belongs to.
pub const NODE_INTERPRETER_STREAM_PROJECT_DIRECTORY_ENVIRONMENT_VARIABLE: &str =
    "STREAMLIB_STREAM_PROJECT_DIRECTORY";

#[cfg(not(target_os = "macos"))]
pub const SURFACE_SHARE_CHANNEL_ENVIRONMENT_VARIABLE: &str = "STREAMLIB_SURFACE_SOCKET";
#[cfg(target_os = "macos")]
pub const SURFACE_SHARE_CHANNEL_ENVIRONMENT_VARIABLE: &str =
    streamlib_surface_client::SURFACE_SHARE_MACH_SERVICE_ENVIRONMENT_VARIABLE;

/// How long the child has to import the user's class, open its ports, run
/// `setup` and report ready before this host gives up and kills it.
///
/// Generous because a cold child imports the whole runtime — but bounded, so a
/// class that blocks at import time fails the graph instead of hanging it.
const REGISTRATION_DEADLINE: Duration = Duration::from_secs(60);

/// How long a reader's host waits for a compile to take up the promotion of a
/// function node that crashed its helper before it starts the next helper
/// without it.
const LONGEST_WAIT_FOR_A_COMPILE_TO_CLAIM_A_FUNCTION_NODE_PROMOTION: Duration =
    Duration::from_secs(30);

/// How many of its reader's frame intervals one function-node call may run
/// before its helper is ended as stalled.
const FUNCTION_NODE_CALL_WATCHDOG_FRAME_INTERVALS: u32 = 3;

/// The least a function-node call may run before it counts as stalled: a
/// reader with no frame interval still has one, and the death poll that reads
/// the breadcrumb runs only every 100 ms.
const FUNCTION_NODE_CALL_WATCHDOG_FLOOR: Duration = Duration::from_millis(250);

/// How long the registration wait parks before it re-reads whether shutdown has
/// begun. A helper still importing then is put on the ladder rather than left
/// holding the app for the rest of its budget.
const REGISTRATION_SHUTDOWN_OBSERVATION_INTERVAL: Duration = Duration::from_millis(50);

const REPLY_DEADLINE: Duration = Duration::from_secs(5);

/// How long a refusal waits, once a helper's process group is down, for the
/// helper's standard output or standard error to close, so what it wrote last
/// is in the refusal.
///
/// Bounded because a descendant that left the group can hold the pipe open
/// past the helper's own exit.
pub(super) const HELPER_PROCESS_STANDARD_STREAM_CLOSE_DEADLINE: Duration = Duration::from_secs(1);

/// How much of a helper's standard error a refusal carries, from the end.
const STANDARD_ERROR_TAIL_BYTES: usize = 16 * 1024;

pub fn node_interpreter_bootstrap_path(lend_directory: &Path) -> PathBuf {
    lend_directory.join(NODE_INTERPRETER_BOOTSTRAP_PATH_IN_THE_LEND_DIRECTORY)
}

/// `PYTHONPATH` for a stream's interpreter: the lend directory, then the
/// project directory, and nothing inherited.
///
/// The lend directory holds `tatolab/runtime/` and no `tatolab/__init__.py`,
/// so PEP 420 merges the lent `tatolab.runtime` with the venv's own
/// `tatolab.stream`. A directory holding the path-list separator is refused by
/// name: Python would split it into entries naming other directories.
pub(crate) fn node_interpreter_python_path(
    lend_directory: &Path,
    project_directory: &Path,
) -> Result<OsString> {
    std::env::join_paths([lend_directory, project_directory]).map_err(|_| {
        let directory_holding_the_separator = [lend_directory, project_directory]
            .into_iter()
            .find(|directory| std::env::join_paths([directory]).is_err())
            .unwrap_or(project_directory);
        Error::Configuration(format!(
            "`{}` cannot be on a stream interpreter's PYTHONPATH: its path holds the \
             path-list separator, which would split it into other directories",
            directory_holding_the_separator.display()
        ))
    })
}

pub(crate) fn stream_interpreter_command_in_its_project_directory(
    interpreter: &Path,
    project_directory: &Path,
    lend_directory: &Path,
) -> Result<Command> {
    let python_path = node_interpreter_python_path(lend_directory, project_directory)?;
    let mut command = Command::new(interpreter);
    command.current_dir(project_directory);
    remove_every_python_variable_named_in(
        &mut command,
        std::env::vars_os().map(|(variable_name, _)| variable_name),
    );
    command.env("PYTHONPATH", python_path);
    start_the_interpreter_at_the_open_descriptor_soft_limit_the_runtime_started_with(&mut command);
    Ok(command)
}

/// Remove from `command` each of `inherited_variable_names` that begins with
/// `PYTHON`.
pub(crate) fn remove_every_python_variable_named_in(
    command: &mut Command,
    inherited_variable_names: impl IntoIterator<Item = OsString>,
) {
    use std::os::unix::ffi::OsStrExt;
    for variable_name in inherited_variable_names {
        if variable_name.as_bytes().starts_with(b"PYTHON") {
            command.env_remove(variable_name);
        }
    }
}

pub(crate) fn node_interpreter_bootstrap_command(
    stream_environment: &StreamEnvironment,
    lend_directory: &Path,
) -> Result<Command> {
    let mut command = stream_interpreter_command_in_its_project_directory(
        &stream_environment.interpreter,
        &stream_environment.project_directory,
        lend_directory,
    )?;
    command
        .arg(node_interpreter_bootstrap_path(lend_directory))
        .env(ENGINE_BUILD_ID_ENVIRONMENT_VARIABLE, ENGINE_BUILD_ID);
    Ok(command)
}

/// How a helper reaches the engine's surface-share service: the variable its
/// exchange client reads, and the socket path or Mach service name it holds.
pub(crate) struct SurfaceShareChannelNamedToTheHelperProcess<'a> {
    pub(crate) environment_variable: &'static str,
    pub(crate) channel_name: &'a std::ffi::OsStr,
}

#[derive(Debug, Clone)]
pub(crate) struct LoadedStreamAHelperProcessBelongsTo {
    pub(crate) stream_tag: LoadedStreamTag,
    pub(crate) shutdown_escalation: ShutdownEscalationOfOneStream,
}

pub(crate) struct NodeInterpreterSpawnHost {
    /// `module:qualname` — what the child imports the class back by, and what
    /// it receives as `STREAMLIB_ENTRYPOINT`.
    node_class_import_path: String,
    node_display_name: String,
    node_run_id: String,
    node_configuration: Option<serde_json::Value>,
    descriptor: NodeDescriptor,
    stream_environment: StreamEnvironment,
    /// The directory holding the `tatolab/runtime/` the child borrows.
    node_interpreter_lend_directory: PathBuf,
    the_stream_this_helper_belongs_to: LoadedStreamAHelperProcessBelongsTo,
    child: Option<Child>,
    iceoryx2_domain_root: Option<PathBuf>,
    child_standard_error_tail: Option<ChildStandardStreamRecording>,
    bridge: Option<SubprocessBridge>,
    child_is_gone: bool,
    /// Why the helper died on its own — its exit and the end of its standard
    /// error — once the engine noticed and took its group down.
    why_the_helper_process_died_on_its_own: Option<String>,
    /// The call to a function node of one of its islands the helper was in
    /// when it died or was ended; that node is promoted out of them before the
    /// next helper starts.
    function_node_call_that_lost_the_helper: Option<FunctionNodeCallUnderWayInTheFarSide>,
    /// Set once this helper has been through the shutdown ask, so the ladder's
    /// own belt-and-braces call cannot make it a second time.
    shutdown_was_already_asked_of_this_helper: bool,
    link_wiring: Arc<OutOfProcessLinkWiringEnvelope>,
    /// The node's catalog entries, which the helper replaces and reads over
    /// its bridge; each helper it starts gets leases of its own.
    what_the_node_reaches: WhatAHelperPlacedNodeReaches,
    /// The helper's admission to the engine's surface-share Mach service,
    /// held until the helper is reaped — from then on its pid may belong to
    /// any process on the machine.
    #[cfg(target_os = "macos")]
    surface_share_admission_of_the_helper_process:
        Option<crate::apple::surface_share::SurfaceShareHelperProcessAdmission>,
}

/// The `rpc` tag a helper answered with, if it wrote one.
fn lifecycle_reply_tag(reply: &serde_json::Value) -> Option<&str> {
    reply.get("rpc").and_then(|rpc| rpc.as_str())
}

/// The reason a helper gave for a refusal, or a stand-in saying it gave none.
fn reported_reason(reply: &serde_json::Value) -> &str {
    reply
        .get("error")
        .and_then(|error| error.as_str())
        .unwrap_or("it reported no reason")
}

/// Wait up to `slice` for the reply this command is answered with, reading
/// past whatever the helper sent ahead of it.
///
/// Draining rather than taking the first frame: both commands are on the wire
/// at once, so the helper answers `stopped` and then `done`, and a rung that
/// claimed whatever arrived first would read the `stop` reply as the teardown
/// it is timing.
fn await_the_reply_to(
    bridge: &SubprocessBridge,
    command: HelperProcessShutdownCommand,
    slice: Duration,
) -> LifecycleReplyAwaited {
    let deadline = Instant::now() + slice;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return LifecycleReplyAwaited::SliceElapsed;
        }
        match bridge.recv_lifecycle_timeout(remaining) {
            Ok(reply) if lifecycle_reply_tag(&reply) == Some(command.reply_tag()) => {
                return LifecycleReplyAwaited::Arrived;
            }
            Ok(_reply_to_an_earlier_command) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                return LifecycleReplyAwaited::SliceElapsed;
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                return LifecycleReplyAwaited::NoReplyCanArrive;
            }
        }
    }
}

fn refusal_of_a_helper_process_that_died_while_setting_up(
    node_display_name: &str,
    standard_error_tail: &str,
) -> Error {
    Error::Runtime(format!(
        "[{node_display_name}] its helper process died before it finished setting up. {}",
        standard_error_tail_as_a_refusal_quotes_it(standard_error_tail)
    ))
}

/// The end of a helper's standard error as a refusal quotes it, saying so when
/// there is none.
pub(super) fn standard_error_tail_as_a_refusal_quotes_it(standard_error_tail: &str) -> String {
    if standard_error_tail.is_empty() {
        return "It wrote nothing to its standard error.".to_string();
    }
    format!("Its standard error ended with:\n{standard_error_tail}")
}

/// How much of a standard output that is not the document a stream
/// interpreter was to print a refusal quotes.
const STREAM_INTERPRETER_STANDARD_OUTPUT_QUOTED_BYTES: usize = 512;

/// The first [`STREAM_INTERPRETER_STANDARD_OUTPUT_QUOTED_BYTES`] of a stream
/// interpreter's standard output, quoted.
pub(super) fn standard_output_head_as_a_refusal_quotes_it(standard_output_bytes: &[u8]) -> String {
    let head = &standard_output_bytes[..standard_output_bytes
        .len()
        .min(STREAM_INTERPRETER_STANDARD_OUTPUT_QUOTED_BYTES)];
    let ellipsis = if head.len() < standard_output_bytes.len() {
        "…"
    } else {
        ""
    };
    format!("{:?}{ellipsis}", String::from_utf8_lossy(head))
}

/// How long the wait for a stream interpreter parks before it re-reads whether
/// it was cut short.
const STREAM_INTERPRETER_EXIT_OBSERVATION_INTERVAL: Duration = Duration::from_millis(50);

/// How the wait for a stream interpreter that runs to its own exit — a
/// describe, a compile — ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum StreamInterpreterExitAwaited {
    /// It exited, and is left unreaped.
    Exited,
    /// Its bound elapsed first.
    BoundElapsed,
    /// The shutdown of the stream it works for was requested first.
    ItsStreamsShutdownRequested,
    /// The runtime's host interrupted it first.
    InterruptedByTheHost,
}

/// Wait up to `bound` for `child` to exit, leaving it unreaped, cut short
/// whenever `is_its_streams_shutdown_requested` or `is_interrupted_by_the_host`
/// reports true; a bound past what an [`Instant`] holds never elapses.
pub(super) fn wait_for_a_stream_interpreter_to_exit(
    child: &Child,
    bound: Duration,
    is_its_streams_shutdown_requested: &dyn Fn() -> bool,
    is_interrupted_by_the_host: &dyn Fn() -> bool,
) -> StreamInterpreterExitAwaited {
    let deadline = Instant::now().checked_add(bound);
    loop {
        let observation_slice =
            deadline.map_or(STREAM_INTERPRETER_EXIT_OBSERVATION_INTERVAL, |deadline| {
                deadline
                    .saturating_duration_since(Instant::now())
                    .min(STREAM_INTERPRETER_EXIT_OBSERVATION_INTERVAL)
            });
        if wait_for_a_child_to_become_collectable_within(child, observation_slice) {
            return StreamInterpreterExitAwaited::Exited;
        }
        if is_its_streams_shutdown_requested() {
            return StreamInterpreterExitAwaited::ItsStreamsShutdownRequested;
        }
        if is_interrupted_by_the_host() {
            return StreamInterpreterExitAwaited::InterruptedByTheHost;
        }
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            return StreamInterpreterExitAwaited::BoundElapsed;
        }
    }
}

/// How a child [`run_a_child_process_to_its_exit_within`] ran ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HowABoundedChildProcessRunEnded {
    /// It exited, successfully or not.
    Exited,
    /// Its bound elapsed first, and it was killed.
    BoundElapsed,
    /// The runtime's host interrupted it first, and it was killed.
    InterruptedByTheHost,
}

/// A child run to its exit within a bound: how the run ended, the status its
/// process-group leader was reaped with, and the tail of its standard error.
#[derive(Debug)]
pub(crate) struct BoundedChildProcessRun {
    /// How the run ended.
    pub(crate) ended: HowABoundedChildProcessRunEnded,
    /// The leader's exit status; `None` when it could not be collected.
    pub(crate) exit_status: Option<std::process::ExitStatus>,
    /// The end of what it wrote to a piped standard error, trimmed; empty when
    /// its standard error was not piped.
    pub(crate) standard_error_tail: String,
}

impl BoundedChildProcessRun {
    /// Whether it exited on its own, successfully.
    pub(crate) fn exited_successfully(&self) -> bool {
        self.ended == HowABoundedChildProcessRunEnded::Exited
            && self
                .exit_status
                .is_some_and(|exit_status| exit_status.success())
    }

    /// Its exit status as a refusal names it.
    pub(crate) fn exit_status_rendered(&self) -> String {
        self.exit_status
            .map(|exit_status| exit_status.to_string())
            .unwrap_or_else(|| "an exit status that could not be collected".to_string())
    }

    /// The end of its standard error as a refusal quotes it.
    pub(crate) fn standard_error_as_a_refusal_quotes_it(&self) -> String {
        standard_error_tail_as_a_refusal_quotes_it(&self.standard_error_tail)
    }
}

/// Spawn `command` outside every iceoryx2 listener bind, keep the tail of a
/// piped standard error — logging each line under `standard_error_log_label` —
/// and wait up to `bound` for it to exit, cut short whenever
/// `is_interrupted_by_the_host` reports true. Its whole process group is killed
/// either way, so `command` must give the child a group of its own.
pub(crate) fn run_a_child_process_to_its_exit_within(
    command: &mut Command,
    standard_error_log_label: &str,
    bound: Duration,
    is_interrupted_by_the_host: &dyn Fn() -> bool,
) -> std::io::Result<BoundedChildProcessRun> {
    run_a_child_process_to_its_exit_within_handing_on_its_standard_error_lines(
        command,
        standard_error_log_label,
        bound,
        is_interrupted_by_the_host,
        None,
    )
}

/// What each line a child writes to its piped standard error is handed to, on the thread
/// that reads it.
pub(crate) type ChildStandardErrorLineTakenIn = Box<dyn FnMut(&str) + Send>;

/// [`run_a_child_process_to_its_exit_within`], handing each line of the child's piped
/// standard error to `each_standard_error_line_taken_in` as it is written.
pub(crate) fn run_a_child_process_to_its_exit_within_handing_on_its_standard_error_lines(
    command: &mut Command,
    standard_error_log_label: &str,
    bound: Duration,
    is_interrupted_by_the_host: &dyn Fn() -> bool,
    each_standard_error_line_taken_in: Option<ChildStandardErrorLineTakenIn>,
) -> std::io::Result<BoundedChildProcessRun> {
    let mut child = crate::iceoryx2::spawn_outside_every_iceoryx2_listener_bind(command)?;
    let standard_error_recording =
        child
            .stderr
            .take()
            .map(|standard_error| match each_standard_error_line_taken_in {
                Some(line_taken_in) => spawn_standard_error_reader_keeping_its_tail(
                    ChildStandardErrorLineHandingReader {
                        child_standard_error: standard_error,
                        partial_line: Vec::new(),
                        line_taken_in,
                    },
                    standard_error_log_label,
                ),
                None => spawn_standard_error_reader_keeping_its_tail(
                    standard_error,
                    standard_error_log_label,
                ),
            });
    let exit_awaited =
        wait_for_a_stream_interpreter_to_exit(&child, bound, &|| false, is_interrupted_by_the_host);
    let exit_status =
        kill_the_process_group_and_reap_its_leader(&mut child, standard_error_log_label);
    let ended = match exit_awaited {
        StreamInterpreterExitAwaited::Exited => HowABoundedChildProcessRunEnded::Exited,
        StreamInterpreterExitAwaited::BoundElapsed => HowABoundedChildProcessRunEnded::BoundElapsed,
        StreamInterpreterExitAwaited::ItsStreamsShutdownRequested
        | StreamInterpreterExitAwaited::InterruptedByTheHost => {
            HowABoundedChildProcessRunEnded::InterruptedByTheHost
        }
    };
    Ok(BoundedChildProcessRun {
        ended,
        exit_status,
        standard_error_tail: standard_error_recording
            .map(|recording| {
                recording.text_once_closed_or_after(HELPER_PROCESS_STANDARD_STREAM_CLOSE_DEADLINE)
            })
            .unwrap_or_default(),
    })
}

/// Which bytes of a child's standard stream a recording keeps.
#[derive(Clone, Copy)]
pub(super) enum ChildStandardStreamKeptBytes {
    /// The first this many; the rest is read and dropped.
    TheFirst(usize),
    /// The last this many, each new byte pushing the oldest out.
    TheLast(usize),
}

/// What a child wrote to one standard stream, bounded, and whether the pipe
/// has closed.
#[derive(Clone)]
pub(super) struct ChildStandardStreamRecording {
    recorded_bytes_and_pipe_closed_signal: Arc<(
        parking_lot::Mutex<RecordedChildStandardStream>,
        parking_lot::Condvar,
    )>,
}

struct RecordedChildStandardStream {
    kept_bytes: ChildStandardStreamKeptBytes,
    recorded_bytes: VecDeque<u8>,
    pipe_closed: bool,
}

impl ChildStandardStreamRecording {
    pub(super) fn keeping(kept_bytes: ChildStandardStreamKeptBytes) -> Self {
        Self {
            recorded_bytes_and_pipe_closed_signal: Arc::new((
                parking_lot::Mutex::new(RecordedChildStandardStream {
                    kept_bytes,
                    recorded_bytes: VecDeque::new(),
                    pipe_closed: false,
                }),
                parking_lot::Condvar::new(),
            )),
        }
    }

    fn record(&self, written_bytes: &[u8]) {
        let (recorded_stream_lock, _) = &*self.recorded_bytes_and_pipe_closed_signal;
        let mut recorded_stream = recorded_stream_lock.lock();
        match recorded_stream.kept_bytes {
            ChildStandardStreamKeptBytes::TheFirst(kept_byte_count) => {
                let room_byte_count =
                    kept_byte_count.saturating_sub(recorded_stream.recorded_bytes.len());
                recorded_stream
                    .recorded_bytes
                    .extend(&written_bytes[..written_bytes.len().min(room_byte_count)]);
            }
            ChildStandardStreamKeptBytes::TheLast(kept_byte_count) => {
                recorded_stream.recorded_bytes.extend(written_bytes);
                let overflow_byte_count = recorded_stream
                    .recorded_bytes
                    .len()
                    .saturating_sub(kept_byte_count);
                recorded_stream.recorded_bytes.drain(..overflow_byte_count);
            }
        }
    }

    fn mark_the_pipe_closed(&self) {
        let (recorded_stream_lock, pipe_closed_signal) =
            &*self.recorded_bytes_and_pipe_closed_signal;
        recorded_stream_lock.lock().pipe_closed = true;
        pipe_closed_signal.notify_all();
    }

    /// What was recorded, once the pipe has closed or `deadline` has passed.
    pub(super) fn bytes_once_closed_or_after(&self, deadline: Duration) -> Vec<u8> {
        let (recorded_stream_lock, pipe_closed_signal) =
            &*self.recorded_bytes_and_pipe_closed_signal;
        let mut recorded_stream = recorded_stream_lock.lock();
        pipe_closed_signal.wait_while_for(
            &mut recorded_stream,
            |recorded_stream| !recorded_stream.pipe_closed,
            deadline,
        );
        recorded_stream.recorded_bytes.make_contiguous().to_vec()
    }

    /// What was recorded as trimmed text, once the pipe has closed or
    /// `deadline` has passed.
    pub(super) fn text_once_closed_or_after(&self, deadline: Duration) -> String {
        String::from_utf8_lossy(&self.bytes_once_closed_or_after(deadline))
            .trim()
            .to_string()
    }
}

/// A reader that records everything read through it, and marks the pipe
/// closed when whatever owns it lets go — at end of file, on a read error, or
/// when its thread never started.
struct ChildStandardStreamRecordingReader<R> {
    child_standard_stream: R,
    recording: ChildStandardStreamRecording,
}

impl<R: Read> Read for ChildStandardStreamRecordingReader<R> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let read_byte_count = self.child_standard_stream.read(buffer)?;
        self.recording.record(&buffer[..read_byte_count]);
        Ok(read_byte_count)
    }
}

impl<R> Drop for ChildStandardStreamRecordingReader<R> {
    fn drop(&mut self) {
        self.recording.mark_the_pipe_closed();
    }
}

/// A reader that hands each whole line read through it, without its line ending, to
/// `line_taken_in`, and the last unended line at end of file.
struct ChildStandardErrorLineHandingReader<R> {
    child_standard_error: R,
    partial_line: Vec<u8>,
    line_taken_in: ChildStandardErrorLineTakenIn,
}

impl<R> ChildStandardErrorLineHandingReader<R> {
    fn hand_on_the_partial_line(&mut self) {
        let line = String::from_utf8_lossy(&self.partial_line);
        (self.line_taken_in)(line.trim_end_matches('\r'));
        self.partial_line.clear();
    }
}

impl<R: Read> Read for ChildStandardErrorLineHandingReader<R> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let read_byte_count = self.child_standard_error.read(buffer)?;
        if read_byte_count == 0 && !self.partial_line.is_empty() {
            self.hand_on_the_partial_line();
        }
        for byte in &buffer[..read_byte_count] {
            if *byte == b'\n' {
                self.hand_on_the_partial_line();
            } else {
                self.partial_line.push(*byte);
            }
        }
        Ok(read_byte_count)
    }
}

/// Log a child's standard error line by line, as every intercepted fd is, while
/// keeping its tail for a refusal.
pub(super) fn spawn_standard_error_reader_keeping_its_tail<R: Read + Send + 'static>(
    standard_error: R,
    node_run_id: &str,
) -> ChildStandardStreamRecording {
    let tail = ChildStandardStreamRecording::keeping(ChildStandardStreamKeptBytes::TheLast(
        STANDARD_ERROR_TAIL_BYTES,
    ));
    spawn_fd_line_reader(
        ChildStandardStreamRecordingReader {
            child_standard_stream: standard_error,
            recording: tail.clone(),
        },
        "py-stderr",
        "fd2",
        node_run_id,
    );
    tail
}

/// Read a child's standard output to its end on a thread of its own, keeping
/// its first `kept_byte_count` bytes and dropping the rest, so the child never
/// blocks on a full pipe.
pub(super) fn spawn_standard_output_reader_keeping_its_head<R: Read + Send + 'static>(
    standard_output: R,
    kept_byte_count: usize,
) -> ChildStandardStreamRecording {
    let head = ChildStandardStreamRecording::keeping(ChildStandardStreamKeptBytes::TheFirst(
        kept_byte_count,
    ));
    let mut recording_reader = ChildStandardStreamRecordingReader {
        child_standard_stream: standard_output,
        recording: head.clone(),
    };
    std::thread::spawn(move || {
        let _ = std::io::copy(&mut recording_reader, &mut std::io::sink());
    });
    head
}

impl Drop for NodeInterpreterSpawnHost {
    fn drop(&mut self) {
        if self.child.is_some() {
            self.take_the_helper_process_group_down();
        }
    }
}

pub(crate) fn spawn_host_for_graph_node(
    node_class_import_path: &str,
    descriptor: &NodeDescriptor,
    child_execution_config: ExecutionConfig,
    node: &GraphNode,
    stream_environment: &StreamEnvironment,
    node_interpreter_lend_directory: &Path,
    the_stream_this_helper_belongs_to: LoadedStreamAHelperProcessBelongsTo,
) -> NodeInterpreterSpawnHost {
    let link_wiring = Arc::new(OutOfProcessLinkWiringEnvelope::for_a_far_side_driven_in(
        child_execution_config.execution,
    ));
    NodeInterpreterSpawnHost {
        node_class_import_path: node_class_import_path.to_string(),
        node_display_name: node.display_name.clone(),
        node_run_id: node.id.to_string(),
        node_configuration: node.config.clone(),
        descriptor: descriptor.clone(),
        stream_environment: stream_environment.clone(),
        node_interpreter_lend_directory: node_interpreter_lend_directory.to_path_buf(),
        the_stream_this_helper_belongs_to,
        child: None,
        iceoryx2_domain_root: None,
        child_standard_error_tail: None,
        bridge: None,
        child_is_gone: false,
        why_the_helper_process_died_on_its_own: None,
        function_node_call_that_lost_the_helper: None,
        shutdown_was_already_asked_of_this_helper: false,
        what_the_node_reaches: WhatAHelperPlacedNodeReaches {
            stream_facts_of_its_output_ports: Arc::clone(node.stream_facts_of_output_ports()),
            upstream_stream_facts_by_its_inbound_links: Arc::clone(
                node.upstream_stream_facts_by_inbound_link(),
            ),
            shared_native_service_leases: Default::default(),
            failure_the_node_reported_of_itself: Default::default(),
            process_raising_the_node_reports: Default::default(),
            node_not_ready_reports: Default::default(),
            output_frames_kept_by_its_output_links: link_wiring
                .output_frames_kept_by_its_output_links(),
            store_of_the_node: None,
        },
        link_wiring,
        #[cfg(target_os = "macos")]
        surface_share_admission_of_the_helper_process: None,
    }
}

pub(crate) fn spawn_host_for_a_function_node_promoted_to_its_own_process(
    function_node_import_path: &str,
    descriptor: &NodeDescriptor,
    node: &GraphNode,
    stream_environment: &StreamEnvironment,
    node_interpreter_lend_directory: &Path,
    the_stream_this_helper_belongs_to: LoadedStreamAHelperProcessBelongsTo,
) -> Result<NodeInterpreterSpawnHost> {
    let promotion = node
        .get::<FunctionNodePromotedToItsOwnProcessComponent>()
        .ok_or_else(|| {
            Error::Configuration(format!(
                "function node `{}` has no process of its own until it is promoted out of its \
                 readers' islands",
                node.id
            ))
        })?;
    let host = spawn_host_for_graph_node(
        function_node_import_path,
        descriptor,
        ExecutionConfig::new(promotion.process_execution),
        node,
        stream_environment,
        node_interpreter_lend_directory,
        the_stream_this_helper_belongs_to,
    );
    host.link_wiring
        .number_the_function_node_its_far_side_runs_as_a_standalone_node(
            node.id.as_str(),
            &node.display_name,
            node.node_uuid,
            descriptor.function_node_state_fields.clone(),
        );
    host.link_wiring
        .tell_each_function_node_how_its_far_side_ran_it([(
            node.id.to_string(),
            Arc::clone(&promotion.how_its_own_helper_ran_it),
        )]);
    Ok(host)
}

#[cfg(test)]
mod tests;
