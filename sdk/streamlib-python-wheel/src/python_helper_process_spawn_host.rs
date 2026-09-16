// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The engine's half of a Python processor: the child it runs in.
//!
//! One of these sits in the graph where the processor does, and owns nothing
//! but the child. It has no mailboxes and no writer — the helper opens its own
//! iceoryx2 ports from the wiring this host forwards — and runs Manual on the
//! engine's side, because the loop that drives the processor is the child's.
//!
//! The spawn target is the app's own interpreter, captured when the `Runtime`
//! was constructed. That is what makes one venv enough: the child is the same
//! Python the app is, with the same packages, reached by exec and never by
//! fork — a forked GPU context is not usable in the child.

use std::collections::VecDeque;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use crate::helper_process_shutdown_ladder::{
    HelperProcessShutdownLadder, HelperProcessShutdownOutcome, LifecycleReplyAwaited,
    a_helper_process_has_exited_without_being_reaped,
};
use pyo3::prelude::*;
use streamlib::sdk::context::{RuntimeContextFullAccess, RuntimeContextLimitedAccess};
use streamlib::sdk::descriptors::ProcessorDescriptor;
use streamlib::sdk::error::{Error, Result};
use streamlib::sdk::execution::{ExecutionConfig, ProcessExecution};
use streamlib::sdk::graph::ProcessorNode;
use streamlib::sdk::helper_process_transport::{
    ENGINE_BUILD_ID, ENGINE_BUILD_ID_ENVIRONMENT_VARIABLE, EscalateTransport,
    HelperProcessShutdownCommand, SETUP_LIFECYCLE_COMMAND_TO_HELPER_PROCESS, SubprocessBridge,
    spawn_fd_line_reader,
};
use streamlib::sdk::iceoryx2::ICEORYX2_DOMAIN_ROOT_ENVIRONMENT_VARIABLE;
use streamlib::sdk::processors::{
    DynGeneratedProcessor, OutOfProcessLinkWireReply, OutOfProcessLinkWiringEnvelope,
};

/// The module CPython is launched with in a helper process.
const HELPER_PROCESS_MODULE: &str = "streamlib._helper";

/// The environment variable carrying the class import path a helper process
/// hosts — set here and nowhere else, so its presence is what tells code
/// running inside a child that it is one.
pub(crate) const HELPER_PROCESS_ENTRYPOINT_ENVIRONMENT_VARIABLE: &str = "STREAMLIB_ENTRYPOINT";

/// The environment variable carrying the id of the processor a helper process hosts.
pub(crate) const HELPER_PROCESS_PROCESSOR_ID_ENVIRONMENT_VARIABLE: &str = "STREAMLIB_PROCESSOR_ID";

/// How long the child has to import the user's class, open its ports, run
/// `setup` and report ready before this host gives up and kills it.
///
/// Generous because a cold child imports the whole wheel — but bounded, so a
/// class that blocks at import time fails the graph instead of hanging it.
const REGISTRATION_DEADLINE: Duration = Duration::from_secs(60);

/// How long the registration wait parks before it re-reads whether shutdown has
/// begun. A helper still importing then is put on the ladder rather than left
/// holding the app for the rest of its budget.
const REGISTRATION_SHUTDOWN_OBSERVATION_INTERVAL: Duration = Duration::from_millis(50);

/// How long a child gets to answer a lifecycle command that is not part of the
/// shutdown ladder before the parent stops waiting on it.
///
/// This bounds the engine's own thread, not the child's work: the callbacks
/// behind these commands are expected to return promptly, and a child that
/// needs longer has already broken the contract. Shutdown has its own budgets —
/// see [`crate::helper_process_shutdown_ladder`], where they are the ladder's
/// rungs rather than one deadline reused.
const REPLY_DEADLINE: Duration = Duration::from_secs(5);

/// How long the refusal of a helper that died while setting up waits for the
/// helper's standard error to close, so what it wrote last is in the refusal.
///
/// Bounded because a descendant the helper started can hold the pipe open past
/// the helper's own exit.
const STANDARD_ERROR_CLOSE_DEADLINE: Duration = Duration::from_secs(1);

/// How much of a helper's standard error a refusal carries, from the end.
const STANDARD_ERROR_TAIL_BYTES: usize = 16 * 1024;

// =============================================================================
// Where a child comes from
// =============================================================================

/// The interpreter a helper process is an exec of, and the directory its
/// imports resolve against.
///
/// Captured once, from the app's own interpreter, rather than resolved per
/// spawn: the promise is that a processor's child is the same Python the app
/// is, and re-deriving that later could pick a different one.
pub(crate) struct HelperProcessLaunchEnvironment {
    pub(crate) interpreter_path: PathBuf,
    /// The directory the app was launched from, carried to the child on
    /// `PYTHONPATH` so a processor module sitting beside the entry file
    /// imports there too.
    pub(crate) app_entry_directory: Option<PathBuf>,
}

fn captured_launch_environment() -> &'static OnceLock<HelperProcessLaunchEnvironment> {
    static CAPTURED_LAUNCH_ENVIRONMENT: OnceLock<HelperProcessLaunchEnvironment> = OnceLock::new();
    &CAPTURED_LAUNCH_ENVIRONMENT
}

/// Read `sys.executable` and the app's entry directory, once per process.
pub(crate) fn capture_helper_process_launch_environment(python: Python<'_>) -> PyResult<()> {
    if captured_launch_environment().get().is_some() {
        return Ok(());
    }
    let sys = python.import("sys")?;
    let interpreter_path = PathBuf::from(sys.getattr("executable")?.extract::<String>()?);
    let app_entry_directory = sys
        .getattr("path")?
        .get_item(0)
        .ok()
        .and_then(|import_root| import_root.extract::<String>().ok())
        .and_then(|import_root| app_import_root_directory(&import_root));
    let _ = captured_launch_environment().set(HelperProcessLaunchEnvironment {
        interpreter_path,
        app_entry_directory,
    });
    Ok(())
}

/// The directory a child should import the app's own modules from.
///
/// `sys.path[0]` rather than `sys.argv[0]`'s parent, because it is the one slot
/// both launch paths agree on: CPython puts the script's directory there for
/// `python app.py`, and `streamlib run` / `dev` inserts the entry file's
/// directory there before executing it. `sys.argv` cannot answer this — the
/// launcher narrows it to the entry file only for the span of that execution
/// and restores its own argv in a `finally`, and the `Runtime` is constructed
/// *after* that, by the launcher calling the app's `setup(rt)`. A child would
/// get the wheel's own package directory and fail to import the app's
/// processors at all.
///
/// Empty is `python -c`'s value for the slot and means the working directory,
/// which the child inherits anyway.
fn app_import_root_directory(import_root: &str) -> Option<PathBuf> {
    if import_root.is_empty() {
        return None;
    }
    Path::new(import_root).canonicalize().ok()
}

pub(crate) fn helper_process_launch_environment() -> Result<&'static HelperProcessLaunchEnvironment>
{
    captured_launch_environment().get().ok_or_else(|| {
        Error::Runtime(
            "no interpreter was captured to spawn helper processes with; a Runtime must exist \
             before a Python processor can be added to a graph"
                .to_string(),
        )
    })
}

/// The engine build id compiled into this extension, which a helper process
/// compares with the id its parent handed it before it opens anything.
#[pyfunction]
pub(crate) fn engine_build_id_compiled_into_this_extension() -> &'static str {
    ENGINE_BUILD_ID
}

// =============================================================================
// The host
// =============================================================================

pub(crate) struct PythonHelperProcessSpawnHostProcessor {
    /// `module:qualname` — what the child imports the class back by, and what
    /// it receives as `STREAMLIB_ENTRYPOINT`.
    processor_class_import_path: String,
    processor_display_name: String,
    processor_id: String,
    processor_configuration: Option<serde_json::Value>,
    descriptor: ProcessorDescriptor,
    /// The mode the *child* drives its processor in. This host is always
    /// Manual on the engine's side.
    child_execution_config: ExecutionConfig,
    interpreter_path: PathBuf,
    app_entry_directory: Option<PathBuf>,
    child: Option<Child>,
    /// The engine-owned iceoryx2 domain this processor's nodes live in, kept
    /// from `setup` because the sweep that reclaims a dead helper's nodes runs
    /// from a liveness poll that is handed no context.
    iceoryx2_domain_root: Option<PathBuf>,
    child_standard_error_tail: Option<HelperProcessStandardErrorTail>,
    bridge: Option<SubprocessBridge>,
    /// Set once the child stops answering. The pipeline keeps running and the
    /// graph shows this processor in error; the frame in flight is lost, and
    /// is never silently replayed.
    child_is_gone: bool,
    /// Set once this helper has been through the shutdown ask, so the ladder's
    /// own belt-and-braces call cannot make it a second time.
    shutdown_was_already_asked_of_this_helper: bool,
    link_wiring: OutOfProcessLinkWiringEnvelope,
}

impl PythonHelperProcessSpawnHostProcessor {
    /// Build the command that becomes the child.
    ///
    /// Separate from the spawn so what a child inherits is assertable without
    /// starting one.
    pub(crate) fn build_helper_process_command(
        &self,
        runtime_id: &str,
        iceoryx2_domain_root: &Path,
        surface_socket_path: Option<&Path>,
    ) -> Command {
        let mut command = Command::new(&self.interpreter_path);
        command
            .arg("-m")
            .arg(HELPER_PROCESS_MODULE)
            // The child never reads stdin; its fd1/fd2 are captured as
            // intercepted log pipes, and the framed protocol rides its own
            // socket so neither can corrupt it.
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // The app's interpreter was found by absolute path, so a
            // PYTHONHOME inherited from a differently-laid-out install would
            // only send the child looking for the wrong standard library.
            .env_remove("PYTHONHOME")
            .env("PYTHONPATH", self.child_python_path())
            .env(
                HELPER_PROCESS_ENTRYPOINT_ENVIRONMENT_VARIABLE,
                &self.processor_class_import_path,
            )
            .env(
                HELPER_PROCESS_PROCESSOR_ID_ENVIRONMENT_VARIABLE,
                &self.processor_id,
            )
            .env("STREAMLIB_RUNTIME_ID", runtime_id)
            .env(
                ICEORYX2_DOMAIN_ROOT_ENVIRONMENT_VARIABLE,
                iceoryx2_domain_root,
            )
            .env(ENGINE_BUILD_ID_ENVIRONMENT_VARIABLE, ENGINE_BUILD_ID);
        if let Some(surface_socket_path) = surface_socket_path {
            command.env("STREAMLIB_SURFACE_SOCKET", surface_socket_path);
        }
        detach_child_from_the_terminal_and_bind_its_lifetime_to_ours(&mut command);
        // Registered before `EscalateTransport::attach`, whose own `pre_exec`
        // clears `FD_CLOEXEC` on the escalate socket's child end: `pre_exec`
        // closures run in registration order, so the one descriptor a helper is
        // owed is handed back after this sweep has marked everything.
        give_the_child_no_descriptor_beyond_stdio(&mut command);
        command
    }

    /// `PYTHONPATH` for the child: the app's entry directory ahead of whatever
    /// this process already carried.
    fn child_python_path(&self) -> String {
        let mut entries: Vec<String> = Vec::new();
        if let Some(app_entry_directory) = self.app_entry_directory.as_ref() {
            entries.push(app_entry_directory.to_string_lossy().into_owned());
        }
        if let Ok(inherited) = std::env::var("PYTHONPATH") {
            if !inherited.is_empty() {
                entries.push(inherited);
            }
        }
        entries.join(":")
    }

    /// The mode string the child drives its own loop in.
    fn child_execution_mode(&self) -> &'static str {
        match self.child_execution_config.execution {
            ProcessExecution::Reactive => "reactive",
            ProcessExecution::Continuous { .. } => "continuous",
            ProcessExecution::Manual => "manual",
        }
    }

    fn send_to_child(&mut self, message: &serde_json::Value) -> Result<()> {
        let bridge = self.bridge.as_ref().ok_or_else(|| {
            Error::Runtime(format!(
                "[{}] there is no helper process to send to",
                self.processor_display_name
            ))
        })?;
        bridge.send(message).inspect_err(|_| {
            self.child_is_gone = true;
        })
    }

    /// Send a command, wait a bounded time for the child's reply, and give up
    /// on the child if either half fails.
    ///
    /// Every command that expects a reply goes through here, so one unusable
    /// child produces one warning rather than a stream of them — and, the
    /// reason the wait is bounded, so no command can park the engine's thread
    /// forever. A child that has died disconnects the channel and is noticed at
    /// once; a child that is *alive but not reading* — a user callback blocked
    /// on a socket, a wedged `teardown` — would never reply, and this runs on
    /// the lifecycle thread holding the processor's lock, so waiting forever
    /// there is a hung `rt.run()`, with the kill that would have resolved it
    /// sitting unreachable further down teardown.
    fn exchange_with_child(
        &mut self,
        message: &serde_json::Value,
        lifecycle_command_name: &str,
    ) -> Option<serde_json::Value> {
        if self.child_is_gone {
            return None;
        }
        if let Err(send_failure) = self.send_to_child(message) {
            tracing::warn!(
                "[{}] helper process stopped listening during {lifecycle_command_name}: \
                 {send_failure}",
                self.processor_display_name
            );
            self.child_is_gone = true;
            return None;
        }
        let reply = self
            .bridge
            .as_ref()
            .map(|bridge| bridge.recv_lifecycle_timeout(REPLY_DEADLINE));
        match reply {
            Some(Ok(reply)) => Some(reply),
            Some(Err(no_reply)) => {
                tracing::warn!(
                    "[{}] helper process did not answer {lifecycle_command_name} within {}s \
                     ({no_reply}); giving up on it",
                    self.processor_display_name,
                    REPLY_DEADLINE.as_secs(),
                );
                self.child_is_gone = true;
                None
            }
            None => None,
        }
    }

    /// Send a command, wait for its reply, and warn if the child answered with
    /// something other than the tag this command expects.
    ///
    /// A mismatch means the child's lifecycle has desynchronized from the
    /// parent's — the reply in hand belongs to an earlier command — which is
    /// worth saying out loud even though there is nothing to do about it here.
    fn exchange_with_child_expecting(
        &mut self,
        message: &serde_json::Value,
        lifecycle_command_name: &str,
        expected_reply_tag: &str,
    ) {
        let Some(reply) = self.exchange_with_child(message, lifecycle_command_name) else {
            return;
        };
        let reply_tag = lifecycle_reply_tag(&reply).unwrap_or("");
        if reply_tag != expected_reply_tag {
            let reported = reported_reason(&reply);
            tracing::warn!(
                "[{}] helper process answered {lifecycle_command_name} with {reply_tag:?} rather \
                 than {expected_reply_tag:?}: {reported}",
                self.processor_display_name
            );
        }
    }

    /// Wait for the child's `ready`, killing it if the deadline passes.
    ///
    /// An unbounded wait here is what turns a class that blocks at import time
    /// into a graph that never comes up and never says why.
    fn await_child_registration(&mut self) -> Result<()> {
        let bridge = self
            .bridge
            .as_ref()
            .ok_or_else(|| Error::Runtime("there is no helper process to wait for".to_string()))?;
        let deadline = Instant::now() + REGISTRATION_DEADLINE;
        // Only a request that arrives *during* this wait cuts it short. The
        // escalation is process-global and taken only when a run ends, so one
        // already raised when a helper starts belongs to a run that has not
        // taken it yet — and reading that as "shutdown began" would refuse
        // every helper a later graph in this process adds.
        let shutdown_was_already_requested =
            streamlib::sdk::runtime::is_runtime_shutdown_requested();
        let reply = loop {
            if !shutdown_was_already_requested
                && streamlib::sdk::runtime::is_runtime_shutdown_requested()
            {
                // A helper still importing when shutdown begins must not hold
                // the app for the rest of a sixty-second budget. It goes on the
                // ladder rather than taking a bare kill, because
                // `docs/plan/ARCHITECTURE.md` §Processor model has any callback
                // interrupted at shutdown — `setup()` included — followed by
                // `teardown()`.
                self.stop_the_helper_process_on_the_shutdown_ladder();
                return Err(Error::Runtime(format!(
                    "[{}] shutdown began while its helper process was still setting up",
                    self.processor_display_name,
                )));
            }
            let remaining = deadline
                .saturating_duration_since(Instant::now())
                .min(REGISTRATION_SHUTDOWN_OBSERVATION_INTERVAL);
            if Instant::now() >= deadline {
                self.take_the_helper_process_group_down();
                return Err(Error::Runtime(format!(
                    "[{}] its helper process did not finish setting up within {}s. The class is \
                     imported from `{}` in a fresh interpreter — work that blocks at import time \
                     blocks here.",
                    self.processor_display_name,
                    REGISTRATION_DEADLINE.as_secs(),
                    self.processor_class_import_path,
                )));
            }
            match bridge.recv_lifecycle_timeout(remaining) {
                Ok(reply) => break reply,
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(self.refuse_the_helper_process_that_died_while_setting_up());
                }
            }
        };

        match lifecycle_reply_tag(&reply) {
            Some("ready") => Ok(()),
            _ => {
                let reported = reported_reason(&reply).to_string();
                // A helper that refused its own setup is never run and never
                // torn down, so this is the only place its group goes.
                self.take_the_helper_process_group_down();
                Err(Error::Runtime(format!(
                    "[{}] could not set itself up in its helper process:\n{reported}",
                    self.processor_display_name
                )))
            }
        }
    }

    /// The refusal of a child that exited before `ready`, carrying the end of
    /// what it wrote to its standard error.
    ///
    /// A helper refuses its own start — an engine build other than the
    /// parent's, a missing variable — before its log channel exists, so raw
    /// standard error is the only place its reason is written.
    fn refuse_the_helper_process_that_died_while_setting_up(&mut self) -> Error {
        // The group first, so nothing the helper started is still holding the
        // standard-error pipe the tail below waits on — and so a refused start
        // leaves no survivor, which `docs/plan/ARCHITECTURE.md` §Processor
        // model owes at every helper exit and not only at the ones that reach
        // teardown.
        self.take_the_helper_process_group_down();
        let standard_error_tail = self
            .child_standard_error_tail
            .as_ref()
            .map(|tail| tail.text_once_closed_or_after(STANDARD_ERROR_CLOSE_DEADLINE))
            .unwrap_or_default();
        refusal_of_a_helper_process_that_died_while_setting_up(
            &self.processor_display_name,
            &standard_error_tail,
        )
    }

    /// Ask the helper for both shutdown rungs at once, which is the plan's
    /// "`stop` and `teardown` are sent together".
    ///
    /// Queued together and timed apart: a helper that misses its `stopped` —
    /// because a callback outran the budget and took the interrupt — already
    /// holds the command that runs its `teardown()`, so the interrupt costs it
    /// the bag in flight rather than its teardown.
    ///
    /// Idempotent, because the engine's `stop()` is not the only way onto the
    /// ladder: a helper interrupted while it was still importing never reached
    /// that hook, and the plan owes it a `teardown()` all the same.
    fn ask_the_helper_to_stop_and_tear_down(&mut self) {
        if self.shutdown_was_already_asked_of_this_helper {
            return;
        }
        self.shutdown_was_already_asked_of_this_helper = true;
        // No channel is no failure to report: a helper whose crash was already
        // noticed had its bridge dropped then, and warning twice more about a
        // command nobody could have taken is the stream of noise
        // `exchange_with_child` exists to avoid. A helper that is merely
        // unresponsive still has its bridge, and is still asked.
        if self.bridge.is_none() {
            return;
        }
        // Warned once rather than per command: a helper that did not take the
        // first will not take the second either, and one unusable child owes
        // one line, which is the posture `exchange_with_child` already keeps.
        for command in HelperProcessShutdownCommand::BOTH_IN_THE_ORDER_THE_LADDER_SENDS_THEM {
            let asked = self.send_to_child(&serde_json::json!({
                "cmd": command.command_tag(),
                "capability": "full",
            }));
            if let Err(unreachable_helper) = asked {
                tracing::warn!(
                    "[{}] its helper process did not take the {} command, so neither it nor \
                     anything after it was asked for: {unreachable_helper}",
                    self.processor_display_name,
                    command.command_tag(),
                );
                return;
            }
        }
    }

    /// Walk the shutdown ladder: a callback that outran its budget interrupted,
    /// `teardown()` given its five seconds, then the helper's whole process
    /// group down and the child reaped.
    fn stop_the_helper_process_on_the_shutdown_ladder(&mut self) {
        self.ask_the_helper_to_stop_and_tear_down();
        let outcome = self.child.take().map(|child| {
            let bridge = self.bridge.as_ref();
            HelperProcessShutdownLadder::taking_over(self.processor_display_name.clone(), child)
                .walk_every_rung(|command, slice| match bridge {
                    Some(bridge) => await_the_reply_to(bridge, command, slice),
                    None => LifecycleReplyAwaited::NoReplyCanArrive,
                })
        });
        self.close_the_engines_end_of_the_helper_process(outcome);
    }

    /// Take the group down with no cooperative rung at all — see
    /// [`HelperProcessShutdownLadder::skip_to_terminating_the_process_group`].
    fn take_the_helper_process_group_down(&mut self) {
        let outcome = self.child.take().map(|child| {
            HelperProcessShutdownLadder::taking_over(self.processor_display_name.clone(), child)
                .skip_to_terminating_the_process_group()
        });
        self.close_the_engines_end_of_the_helper_process(outcome);
    }

    /// Let go of everything this end held for the helper, whichever way it went.
    ///
    /// Dropping the bridge shuts the engine's end of the escalate socket, so a
    /// descendant that outlived the group kill can issue no privileged
    /// operation. The standard-output and standard-error readers are left
    /// running rather than closed: they are detached threads, so nothing waits
    /// on them, and a survivor's writes are still logged instead of raising
    /// SIGPIPE at it.
    fn close_the_engines_end_of_the_helper_process(
        &mut self,
        outcome: Option<HelperProcessShutdownOutcome>,
    ) {
        self.child_is_gone = true;
        self.bridge.take();
        if let Some(HelperProcessShutdownOutcome::Reaped(exit_status)) = outcome {
            tracing::debug!(
                "[{}] helper process exited: {exit_status}",
                self.processor_display_name
            );
        }
        if outcome.is_some() {
            self.reclaim_the_iceoryx2_nodes_the_helper_left();
        }
    }

    /// Reclaim the iceoryx2 nodes a helper left registered.
    ///
    /// A dead node holds its slot in every service it had opened, so a channel
    /// whose destination is gone keeps counting it against the cap until a
    /// sweep takes it out. Run at every helper exit and not only at a detected
    /// crash: a helper the ladder had to kill never finalized its interpreter,
    /// so its engine half never dropped the node either.
    ///
    /// The engine's own domain configuration, never the ambient one: the global
    /// lookup path would sweep another domain, or none.
    fn reclaim_the_iceoryx2_nodes_the_helper_left(&self) {
        let Some(iceoryx2_domain_root) = self.iceoryx2_domain_root.as_deref() else {
            return;
        };
        match streamlib::sdk::iceoryx2::reclaim_dead_iceoryx2_nodes_in_engine_owned_domain(
            iceoryx2_domain_root,
        ) {
            Ok(reclaimed_node_count) if reclaimed_node_count > 0 => tracing::info!(
                "[{}] reclaimed {reclaimed_node_count} iceoryx2 node(s) its helper left",
                self.processor_display_name,
            ),
            Ok(_) => {}
            Err(sweep_failure) => tracing::warn!(
                "[{}] could not reclaim the iceoryx2 nodes its helper left: {sweep_failure}",
                self.processor_display_name,
            ),
        }
    }
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

/// Refuse a processor whose helper process exited before it reported `ready`.
fn refusal_of_a_helper_process_that_died_while_setting_up(
    processor_display_name: &str,
    standard_error_tail: &str,
) -> Error {
    if standard_error_tail.is_empty() {
        return Error::Runtime(format!(
            "[{processor_display_name}] its helper process died before it finished setting up, \
             and wrote nothing to its standard error"
        ));
    }
    Error::Runtime(format!(
        "[{processor_display_name}] its helper process died before it finished setting up. Its \
         standard error ended with:\n{standard_error_tail}"
    ))
}

// =============================================================================
// What a child last wrote to its standard error
// =============================================================================

/// The end of what a helper process wrote to its standard error, and whether
/// the pipe has closed.
#[derive(Clone, Default)]
struct HelperProcessStandardErrorTail {
    recorded_standard_error_and_pipe_closed_signal: Arc<(
        parking_lot::Mutex<RecordedHelperProcessStandardError>,
        parking_lot::Condvar,
    )>,
}

#[derive(Default)]
struct RecordedHelperProcessStandardError {
    tail_bytes: VecDeque<u8>,
    pipe_closed: bool,
}

impl HelperProcessStandardErrorTail {
    fn record(&self, written_bytes: &[u8]) {
        let (recorded_standard_error_lock, _) =
            &*self.recorded_standard_error_and_pipe_closed_signal;
        let mut recorded_standard_error = recorded_standard_error_lock.lock();
        recorded_standard_error.tail_bytes.extend(written_bytes);
        let overflow_byte_count = recorded_standard_error
            .tail_bytes
            .len()
            .saturating_sub(STANDARD_ERROR_TAIL_BYTES);
        recorded_standard_error
            .tail_bytes
            .drain(..overflow_byte_count);
    }

    fn mark_the_pipe_closed(&self) {
        let (recorded_standard_error_lock, pipe_closed_signal) =
            &*self.recorded_standard_error_and_pipe_closed_signal;
        recorded_standard_error_lock.lock().pipe_closed = true;
        pipe_closed_signal.notify_all();
    }

    /// What was recorded, once the pipe has closed or `deadline` has passed.
    fn text_once_closed_or_after(&self, deadline: Duration) -> String {
        let (recorded_standard_error_lock, pipe_closed_signal) =
            &*self.recorded_standard_error_and_pipe_closed_signal;
        let mut recorded_standard_error = recorded_standard_error_lock.lock();
        pipe_closed_signal.wait_while_for(
            &mut recorded_standard_error,
            |recorded_standard_error| !recorded_standard_error.pipe_closed,
            deadline,
        );
        String::from_utf8_lossy(recorded_standard_error.tail_bytes.make_contiguous())
            .trim()
            .to_string()
    }
}

/// A reader that records the tail of everything read through it, and marks the
/// pipe closed when the line reader owning it lets go — at end of file, on a
/// read error, or when its thread never started.
struct StandardErrorTailRecordingReader<R> {
    standard_error: R,
    tail: HelperProcessStandardErrorTail,
}

impl<R: Read> Read for StandardErrorTailRecordingReader<R> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let read_byte_count = self.standard_error.read(buffer)?;
        self.tail.record(&buffer[..read_byte_count]);
        Ok(read_byte_count)
    }
}

impl<R> Drop for StandardErrorTailRecordingReader<R> {
    fn drop(&mut self) {
        self.tail.mark_the_pipe_closed();
    }
}

/// Log a child's standard error line by line, as every intercepted fd is, while
/// keeping its tail for a refusal.
fn spawn_standard_error_reader_keeping_its_tail<R: Read + Send + 'static>(
    standard_error: R,
    processor_id: &str,
) -> HelperProcessStandardErrorTail {
    let tail = HelperProcessStandardErrorTail::default();
    spawn_fd_line_reader(
        StandardErrorTailRecordingReader {
            standard_error,
            tail: tail.clone(),
        },
        "py-stderr",
        "fd2",
        processor_id,
    );
    tail
}

/// Give the child its own process group and tie its lifetime to this process.
///
/// The process group is what keeps a terminal Ctrl-C from reaching children
/// directly: the signal goes to the app, and children come down through the
/// teardown the app then runs, having had a chance to release what they hold.
/// `PR_SET_PDEATHSIG` covers the case the app never gets to run teardown —
/// a `SIGKILL`ed parent leaves no orphans — and the `getppid` recheck closes
/// the window where the parent died between fork and that call, which would
/// otherwise arm a signal that never fires.
fn detach_child_from_the_terminal_and_bind_its_lifetime_to_ours(command: &mut Command) {
    use std::os::unix::process::CommandExt;

    let spawning_process_id = std::process::id() as libc::pid_t;
    // SAFETY: everything called here is async-signal-safe, which is the
    // contract for a `pre_exec` closure running between fork and exec.
    unsafe {
        command.pre_exec(move || {
            if libc::setpgid(0, 0) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            #[cfg(target_os = "linux")]
            {
                if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                if libc::getppid() != spawning_process_id {
                    libc::_exit(1);
                }
            }
            Ok(())
        });
    }
}

/// The first descriptor past standard input, output and error.
const FIRST_DESCRIPTOR_PAST_STDIO: libc::c_uint = 3;

/// How far the per-descriptor fallback walks when the process's own limit is
/// higher than that.
///
/// A session's limit can be a million, and a million `fcntl` calls between fork
/// and exec would cost more than the interpreter start they precede. A
/// descriptor above this is the stated residual of the fallback arm.
const DESCRIPTOR_SWEEP_FALLBACK_CEILING: u64 = 65_536;

/// Give the child no descriptor beyond its standard streams.
///
/// `docs/plan/ARCHITECTURE.md` §Processor model: a helper inherits nothing but
/// its escalate socket and its standard streams. Everything else this process
/// holds — the stdio interceptor's four dups and two pipe read ends, another
/// helper's surface-share dups — reaches a child today, and a grandchild
/// holding one keeps the app's output open past the app's own exit. That is the
/// "won't quit even with SIGKILL" shape.
///
/// Marked close-on-exec rather than closed, because std's own machinery is
/// still using descriptors here: the pipe it reports a failed `exec` on is one
/// of them, and closing it would make a failure to start read as a success.
fn give_the_child_no_descriptor_beyond_stdio(command: &mut Command) {
    use std::os::unix::process::CommandExt;

    // Read here rather than in the child: `getrlimit` is not on POSIX's
    // async-signal-safe list, and the answer cannot change for the child
    // between this call and its exec.
    let highest_descriptor_the_fallback_walks = highest_descriptor_the_fallback_walks();

    let sweep_every_descriptor_past_stdio = move || {
        if one_syscall_marked_every_descriptor_past_stdio_close_on_exec() {
            return Ok(());
        }
        // SAFETY: this closure is the `pre_exec` the call below registers, and
        // `fcntl` is all the fallback reaches for.
        unsafe {
            mark_each_descriptor_past_stdio_close_on_exec(highest_descriptor_the_fallback_walks)
        };
        Ok(())
    };
    // SAFETY: the closure calls only the `close_range` syscall and `fcntl`,
    // both async-signal-safe, which is the contract for a `pre_exec` closure
    // running between fork and exec.
    unsafe {
        command.pre_exec(sweep_every_descriptor_past_stdio);
    }
}

/// How far the per-descriptor fallback walks in this process, read before the
/// fork so the child never has to ask.
fn highest_descriptor_the_fallback_walks() -> libc::c_uint {
    // SAFETY: a zeroed `rlimit` is a valid buffer for `getrlimit` to fill.
    let mut descriptor_limit: libc::rlimit = unsafe { std::mem::zeroed() };
    // SAFETY: `descriptor_limit` is a valid, live `rlimit` for the duration.
    let read = unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut descriptor_limit) };
    let limit = if read == 0 {
        // Clamped in the wide type: `rlim_cur` is 64-bit and `RLIM_INFINITY`
        // is its maximum, so narrowing first would wrap a large real limit
        // down to a small one and stop the sweep short.
        (descriptor_limit.rlim_cur as u64).min(DESCRIPTOR_SWEEP_FALLBACK_CEILING)
    } else {
        DESCRIPTOR_SWEEP_FALLBACK_CEILING
    };
    limit as libc::c_uint
}

/// Ask the kernel to mark the whole range at once, and say whether it did.
///
/// The raw syscall rather than glibc's `close_range` wrapper: that symbol
/// arrived in glibc 2.34, and the release wheel is linked inside
/// `manylinux_2_28`, whose glibc is 2.28 — an extern reference would fail to
/// link there while every CI runner, on a newer glibc, links it happily. The
/// syscall itself needs only Linux 5.9, and 5.11 for the flag.
#[cfg(target_os = "linux")]
fn one_syscall_marked_every_descriptor_past_stdio_close_on_exec() -> bool {
    // SAFETY: a raw syscall with scalar arguments, async-signal-safe.
    let swept = unsafe {
        libc::syscall(
            libc::SYS_close_range,
            libc::c_long::from(FIRST_DESCRIPTOR_PAST_STDIO),
            libc::c_long::from(libc::c_uint::MAX),
            libc::c_long::from(libc::CLOSE_RANGE_CLOEXEC),
        )
    };
    swept == 0
}

/// macOS has no `close_range`, so its children always take the fallback.
#[cfg(not(target_os = "linux"))]
fn one_syscall_marked_every_descriptor_past_stdio_close_on_exec() -> bool {
    false
}

/// Mark each descriptor past stdio close-on-exec, one at a time.
///
/// Reached on macOS always, and on a Linux kernel older than the
/// `CLOSE_RANGE_CLOEXEC` flag.
///
/// # Safety
///
/// Called only from a `pre_exec` closure. `fcntl` is on POSIX's
/// async-signal-safe list, which is what makes that legal.
unsafe fn mark_each_descriptor_past_stdio_close_on_exec(highest_descriptor: libc::c_uint) {
    for descriptor in FIRST_DESCRIPTOR_PAST_STDIO..highest_descriptor {
        let flags = unsafe { libc::fcntl(descriptor as libc::c_int, libc::F_GETFD) };
        if flags < 0 {
            continue;
        }
        unsafe {
            libc::fcntl(
                descriptor as libc::c_int,
                libc::F_SETFD,
                flags | libc::FD_CLOEXEC,
            )
        };
    }
}

impl DynGeneratedProcessor for PythonHelperProcessSpawnHostProcessor {
    fn __generated_setup(&mut self, ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        #[cfg(target_os = "linux")]
        let surface_socket_path = Some(ctx.surface_socket_path());
        #[cfg(not(target_os = "linux"))]
        let surface_socket_path: Option<&Path> = None;

        let iceoryx2_domain_root = ctx.runtime_directory().iceoryx2_domain_root();
        let mut command = self.build_helper_process_command(
            &ctx.runtime_id(),
            &iceoryx2_domain_root,
            surface_socket_path,
        );
        self.iceoryx2_domain_root = Some(iceoryx2_domain_root);
        let mut escalate_transport = EscalateTransport::attach(&mut command)?;

        let mut child = command.spawn().map_err(|spawn_failure| {
            Error::Runtime(format!(
                "[{}] could not start its helper process with `{} -m {HELPER_PROCESS_MODULE}`: \
                 {spawn_failure}",
                self.processor_display_name,
                self.interpreter_path.display(),
            ))
        })?;
        // After the spawn, so the child is the only holder of its end and sees
        // EOF when this process lets go.
        escalate_transport.release_child_end();

        tracing::info!(
            "[{}] helper process started: pid={}, entrypoint={}",
            self.processor_display_name,
            child.id(),
            self.processor_class_import_path,
        );
        // `pre_exec` made the child the leader of a group whose id is its pid.
        if !streamlib::sdk::runtime::register_a_helper_process_group(child.id() as i32) {
            tracing::warn!(
                "[{}] its helper process group could not be registered, so a third interrupt \
                 will not kill it; the kernel still kills the helper itself when the app exits",
                self.processor_display_name,
            );
        }

        // fd1/fd2 carry anything that bypasses `streamlib.log` — a raw
        // `os.write`, a C extension's `printf`, an interpreter-level fatal —
        // and each line becomes an `intercepted` record in the unified JSONL.
        if let Some(child_stdout) = child.stdout.take() {
            spawn_fd_line_reader(child_stdout, "py-stdout", "fd1", &self.processor_id);
        }
        if let Some(child_stderr) = child.stderr.take() {
            self.child_standard_error_tail = Some(spawn_standard_error_reader_keeping_its_tail(
                child_stderr,
                &self.processor_id,
            ));
        }

        self.child = Some(child);
        self.bridge = Some(SubprocessBridge::new(
            escalate_transport.into_parent_stream(),
            ctx.gpu_limited_access().clone(),
            self.processor_id.clone(),
        )?);

        let setup_command_sent = self.send_to_child(&serde_json::json!({
            // The engine's own constant: the escalate dispatch reads this
            // exact spelling to decide that a window may be minted, and a
            // rename on one side alone would refuse every window silently.
            "cmd": SETUP_LIFECYCLE_COMMAND_TO_HELPER_PROCESS,
            "capability": "full",
            "config": self
                .processor_configuration
                .clone()
                .unwrap_or(serde_json::Value::Null),
            "processor_id": self.processor_id,
            "ports": self.link_wiring.as_setup_command_ports(),
        }));
        if let Err(setup_command_send_failure) = setup_command_sent {
            // The child's end is already closed: it refused its own start
            // before reading anything.
            tracing::debug!(
                "[{}] could not send its helper process the setup command: \
                 {setup_command_send_failure}",
                self.processor_display_name
            );
            return Err(self.refuse_the_helper_process_that_died_while_setting_up());
        }
        self.await_child_registration()
    }

    fn start(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        if self.child_is_gone {
            return Ok(());
        }
        // `run` enters the child's execution loop and is deliberately
        // unanswered — a reply would be read as the answer to the next command.
        self.send_to_child(&serde_json::json!({
            "cmd": "run",
            "capability": "limited",
            "execution": self.child_execution_mode(),
            "interval_ms": self
                .child_execution_config
                .execution
                .interval_ms()
                .unwrap_or(0),
        }))
    }

    fn stop(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        self.ask_the_helper_to_stop_and_tear_down();
        Ok(())
    }

    fn __generated_teardown(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        self.stop_the_helper_process_on_the_shutdown_ladder();
        Ok(())
    }

    fn __generated_on_pause(&mut self, _ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        self.exchange_with_child_expecting(
            &serde_json::json!({"cmd": "on_pause", "capability": "limited"}),
            "on_pause",
            "ok",
        );
        Ok(())
    }

    fn __generated_on_resume(&mut self, _ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        self.exchange_with_child_expecting(
            &serde_json::json!({"cmd": "on_resume", "capability": "limited"}),
            "on_resume",
            "ok",
        );
        Ok(())
    }

    fn process(&mut self, _ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        // Never called: this host is Manual, and the loop that calls the
        // processor's `process` is the child's own.
        Ok(())
    }

    fn name(&self) -> &str {
        &self.processor_display_name
    }

    fn descriptor(&self) -> Option<ProcessorDescriptor> {
        Some(self.descriptor.clone())
    }

    fn execution_config(&self) -> ExecutionConfig {
        ExecutionConfig::new(ProcessExecution::Manual)
    }

    /// Notice a helper process that died on its own, and take its group with it.
    ///
    /// Asked of the process rather than of the escalate socket, which is what
    /// `docs/plan/ARCHITECTURE.md` §Processor model means by "a crash the engine
    /// detects by the process itself rather than by its socket": a descendant
    /// holding that socket keeps its EOF from ever arriving, so a helper whose
    /// pid is gone went on reading as alive and issuing its privileged
    /// operations.
    fn detect_and_clean_up_after_an_out_of_process_helper_that_died(&mut self) {
        if self.child_is_gone {
            return;
        }
        let Some(child) = self.child.as_ref() else {
            return;
        };
        if !a_helper_process_has_exited_without_being_reaped(child.id()) {
            return;
        }
        tracing::error!(
            "[{}] its helper process (pid={}) died; taking its process group down with it",
            self.processor_display_name,
            child.id(),
        );
        self.take_the_helper_process_group_down();
    }

    fn has_failed_unrecoverably(&self) -> bool {
        // The bridge as well as this host's own flag: between `run` and
        // teardown the parent sends nothing, so a child that dies mid-run is
        // never noticed by a failed exchange. The bridge's reader thread sees
        // EOF the moment the child's *last* fd closes, which a surviving
        // descendant defers indefinitely — so the poll above asks the process
        // and this reads what either of them found.
        self.child_is_gone || self.bridge.as_ref().is_some_and(|bridge| bridge.is_dead())
    }

    fn has_iceoryx2_outputs(&self) -> bool {
        false
    }

    fn has_iceoryx2_inputs(&self) -> bool {
        false
    }

    fn out_of_process_link_wiring(&mut self) -> Option<&mut OutOfProcessLinkWiringEnvelope> {
        Some(&mut self.link_wiring)
    }

    /// Tell the child to drop the port it opened for a disconnected link.
    ///
    /// Unanswered, like `run`. The compiler calls this holding the graph's
    /// write lock, so waiting out `REPLY_DEADLINE` on a child that is busy in
    /// a callback would park every other graph operation behind it — and a
    /// reply nobody reads is read as the answer to the next command, which is
    /// the desync `exchange_with_child_expecting` warns about.
    ///
    /// A child that is not there needs no telling, and that is the ordinary
    /// case rather than a failure — its ports went with the process, or were
    /// never opened. Three windows: before `setup` builds the bridge, after
    /// `teardown` takes it, and any point a child died on its own, which
    /// leaves the bridge in place and is noticed only by its reader thread
    /// seeing EOF. Reporting any of them as a refused reclaim would put a
    /// leak warning in front of an operator on a clean shutdown.
    fn unwire_out_of_process_link(
        &mut self,
        port_direction: streamlib::sdk::error::PortDirection,
        local_port_name: &str,
        link_id: &str,
    ) -> Result<()> {
        if let Some(bridge) = self.bridge.as_ref() {
            // A link on its way out is one this child owes no answer for. Left
            // waiting, a child that dies later would refuse a link the graph no
            // longer has.
            bridge.stop_awaiting_the_subprocess_wire_answer_for_link(link_id);
        }
        if self.has_failed_unrecoverably() || self.bridge.is_none() {
            return Ok(());
        }
        self.send_to_child(&serde_json::json!({
            "cmd": "unwire_link",
            "direction": port_direction.as_wire_str(),
            "port": local_port_name,
            "link_id": link_id,
        }))
    }

    /// Hand the child one link wired after its setup, and hand back the cell
    /// its answer will land in.
    ///
    /// The send itself does not wait — the compiler calls this holding the
    /// graph's write lock, and a child reads commands only between callbacks,
    /// so waiting would park every other graph operation behind user code. The
    /// child answers on its own rpc tag, which the bridge routes to this cell,
    /// and `graph` is where the caller reads the outcome.
    ///
    /// Before `setup` there is no bridge and nothing to send: the setup command
    /// reads the envelope, which already carries this link, and the child's
    /// `ready` confirms it — so that link waits on no cell of its own. A child
    /// that has died is refused instead, so the compile wiring the link fails
    /// and its caller hears it rather than reading a wired link nothing will
    /// cross.
    fn wire_out_of_process_link(
        &mut self,
        port_direction: streamlib::sdk::error::PortDirection,
        link_wiring: &serde_json::Value,
    ) -> Result<Option<Arc<OutOfProcessLinkWireReply>>> {
        if self.has_failed_unrecoverably() {
            return Err(Error::Runtime(format!(
                "processor '{}' ({}) has failed, so no link can be wired into it",
                self.processor_display_name, self.processor_id
            )));
        }
        let Some(bridge) = self.bridge.as_ref() else {
            return Ok(None);
        };
        let Some(link_id) = link_wiring.get("link_id").and_then(|id| id.as_str()) else {
            return Err(Error::Configuration(format!(
                "the wiring handed to processor '{}' ({}) names no link, so its helper \
                 process could not answer for one",
                self.processor_display_name, self.processor_id
            )));
        };
        let reply = OutOfProcessLinkWireReply::awaiting_the_far_sides_answer();
        // Registered before the send, never after: the child can answer the
        // moment the frame lands, and a cell registered afterwards would miss
        // an answer already routed.
        bridge.await_the_subprocess_wire_answer_for_link(link_id.to_string(), Arc::clone(&reply));
        match self.send_to_child(&serde_json::json!({
            "cmd": "wire_link",
            "direction": port_direction.as_wire_str(),
            "link": link_wiring,
        })) {
            Ok(()) => Ok(Some(reply)),
            Err(send_failure) => {
                // A send that never left is an answer that never comes. The
                // caller hears the failure, and the cell is taken back out so
                // a later death refuses nothing on this link's behalf.
                if let Some(bridge) = self.bridge.as_ref() {
                    bridge.stop_awaiting_the_subprocess_wire_answer_for_link(link_id);
                }
                Err(send_failure)
            }
        }
    }

    fn set_iceoryx2_resources(
        &mut self,
        _output_writer: Option<streamlib::sdk::iceoryx2::OutputWriter>,
        _input_mailboxes: Option<streamlib::sdk::iceoryx2::InputMailboxes>,
    ) -> Result<()> {
        Ok(())
    }

    fn iceoryx2_output_writer_inner(
        &self,
    ) -> Option<std::sync::Arc<streamlib::sdk::iceoryx2::OutputWriterInner>> {
        None
    }

    fn iceoryx2_input_mailboxes_inner(
        &self,
    ) -> Option<std::sync::Arc<streamlib::sdk::iceoryx2::InputMailboxesInner>> {
        None
    }

    fn apply_config_json(&mut self, config_json: &serde_json::Value) -> Result<()> {
        // Unlike the lifecycle hooks, which swallow a lost child because
        // teardown must proceed regardless, a config update has no such
        // constraint — reporting success while the child runs the old
        // configuration would be a lie the control plane passes on.
        let reply = self
            .exchange_with_child(
                &serde_json::json!({"cmd": "update_config", "config": config_json}),
                "update_config",
            )
            .ok_or_else(|| {
                Error::Runtime(format!(
                    "[{}] its helper process did not answer update_config",
                    self.processor_display_name
                ))
            })?;
        if lifecycle_reply_tag(&reply) != Some("ok") {
            let reported = reported_reason(&reply);
            return Err(Error::Runtime(format!(
                "[{}] its helper process refused update_config: {reported}",
                self.processor_display_name
            )));
        }
        Ok(())
    }

    fn to_runtime_json(&self) -> serde_json::Value {
        serde_json::json!({
            "helper_process_pid": self.child.as_ref().map(|child| child.id()),
            "entrypoint": self.processor_class_import_path,
            "interpreter": self.interpreter_path.to_string_lossy(),
            "helper_process_is_gone": self.child_is_gone,
        })
    }

    fn config_json(&self) -> serde_json::Value {
        self.processor_configuration
            .clone()
            .unwrap_or(serde_json::Value::Null)
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

impl Drop for PythonHelperProcessSpawnHostProcessor {
    /// Last resort for the paths teardown never reached — a failed graph
    /// compile, a panic. A child outliving its host would hold this
    /// processor's iceoryx2 ports open against the next run.
    fn drop(&mut self) {
        if self.child.is_some() {
            self.take_the_helper_process_group_down();
        }
    }
}

/// Build the host for one graph node.
pub(crate) fn spawn_host_for_processor_node(
    processor_class_import_path: &str,
    descriptor: &ProcessorDescriptor,
    child_execution_config: ExecutionConfig,
    node: &ProcessorNode,
) -> Result<PythonHelperProcessSpawnHostProcessor> {
    let launch_environment = helper_process_launch_environment()?;
    Ok(PythonHelperProcessSpawnHostProcessor {
        processor_class_import_path: processor_class_import_path.to_string(),
        processor_display_name: node.display_name.clone(),
        processor_id: node.id.to_string(),
        processor_configuration: node.config.clone(),
        descriptor: descriptor.clone(),
        child_execution_config,
        interpreter_path: launch_environment.interpreter_path.clone(),
        app_entry_directory: launch_environment.app_entry_directory.clone(),
        child: None,
        iceoryx2_domain_root: None,
        child_standard_error_tail: None,
        bridge: None,
        child_is_gone: false,
        shutdown_was_already_asked_of_this_helper: false,
        link_wiring: OutOfProcessLinkWiringEnvelope::default(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

    // =========================================================================
    // What a child inherits
    // =========================================================================

    /// A helper that forks a worker and leaves at once. The worker outlives it
    /// holding whatever it inherited, which is the survivor the descriptor
    /// sweep exists to leave empty-handed.
    const A_HELPER_THAT_FORKS_A_WORKER_AND_LEAVES: &str = r#"
import os, time
if os.fork() == 0:
    time.sleep(30)
    os._exit(0)
"#;

    /// A pipe whose write end is inheritable on purpose, standing in for any
    /// descriptor this process holds that a helper must not keep.
    fn an_inheritable_pipe() -> (OwnedFd, OwnedFd) {
        let mut ends: [libc::c_int; 2] = [-1, -1];
        // SAFETY: `ends` is a two-element array, which is what `pipe2` fills.
        assert_eq!(
            unsafe { libc::pipe2(ends.as_mut_ptr(), libc::O_CLOEXEC) },
            0,
            "pipe2"
        );
        // SAFETY: `ends[1]` was just created here; clearing its close-on-exec
        // flag is what makes it the inheritable descriptor the sweep must catch.
        assert_eq!(
            unsafe { libc::fcntl(ends[1], libc::F_SETFD, 0) },
            0,
            "F_SETFD"
        );
        // SAFETY: both descriptors are freshly created and owned by nobody else.
        unsafe { (OwnedFd::from_raw_fd(ends[0]), OwnedFd::from_raw_fd(ends[1])) }
    }

    /// Whether the descriptor reports end of file inside `budget`, which it can
    /// only do once every holder of the other end has let go.
    fn a_descriptor_reports_end_of_file_within(raw_fd: libc::c_int, budget: Duration) -> bool {
        let mut watched = libc::pollfd {
            fd: raw_fd,
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: one valid `pollfd` for one descriptor this test owns.
        let ready = unsafe { libc::poll(&mut watched, 1, budget.as_millis() as libc::c_int) };
        ready > 0 && watched.revents & (libc::POLLHUP | libc::POLLIN) != 0
    }

    /// Wait for `process_id` to become a zombie, so a test asserting what the
    /// notice says is not racing the child's own exit.
    fn a_helper_process_becomes_collectable_within(process_id: u32, budget: Duration) -> bool {
        let deadline = Instant::now() + budget;
        while Instant::now() < deadline {
            if a_helper_process_has_exited_without_being_reaped(process_id) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        false
    }

    #[test]
    fn nothing_a_helper_starts_can_hold_the_apps_output_open_past_its_own_exit() {
        // The plan's own claim, asserted the way it bites: the app's standard
        // output is a pipe somebody reads to its end, and a grandchild holding
        // a copy of the write end keeps that read waiting after the app is gone.
        let (read_end, write_end) = an_inheritable_pipe();

        let mut command = Command::new("python3");
        command
            .arg("-c")
            .arg(A_HELPER_THAT_FORKS_A_WORKER_AND_LEAVES)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        detach_child_from_the_terminal_and_bind_its_lifetime_to_ours(&mut command);
        give_the_child_no_descriptor_beyond_stdio(&mut command);

        let mut child = command.spawn().expect("the stub helper to start");
        let helper_process_id = child.id() as libc::pid_t;
        // Only the fork's survivor can be holding it now.
        drop(write_end);
        let _ = child.wait();

        let the_apps_output_closed =
            a_descriptor_reports_end_of_file_within(read_end.as_raw_fd(), Duration::from_secs(2));

        // SAFETY: the group is this test's own child's; the survivor is in it.
        unsafe { libc::killpg(helper_process_id, libc::SIGKILL) };

        assert!(
            the_apps_output_closed,
            "a worker the helper forked inherited the app's output and held it open"
        );
    }

    /// Set only in the child process the sweep-placement test re-runs itself in.
    const DEAD_NODE_CHILD_DOMAIN_ROOT_ENVIRONMENT_VARIABLE: &str =
        "STREAMLIB_TEST_HOST_SWEEP_CHILD_ICEORYX2_DOMAIN_ROOT";

    #[test]
    fn a_helper_exit_reclaims_the_iceoryx2_nodes_it_left_whatever_ended_it() {
        // A node is dead only once the process holding it is gone, so it is
        // opened in a child test process that is then killed where it stands.
        if let Some(domain_root) =
            std::env::var_os(DEAD_NODE_CHILD_DOMAIN_ROOT_ENVIRONMENT_VARIABLE)
        {
            let _node = streamlib::sdk::iceoryx2::Iceoryx2Node::new(
                Path::new(&domain_root),
                "streamlib-test/host-sweep-placement",
            )
            .expect("a node opens in the engine-owned domain");
            // SAFETY: this process signalling itself, which is what leaves the
            // node registered with no process behind it.
            unsafe { libc::kill(std::process::id() as libc::pid_t, libc::SIGKILL) };
            unreachable!("SIGKILL to self does not return");
        }

        // Named from this test process's own pid rather than through a
        // temp-directory crate, so the one test needing a private domain adds
        // no dependency to the wheel.
        let domain =
            std::env::temp_dir().join(format!("streamlib-host-sweep-{}", std::process::id()));
        let domain_root = domain.join("iox2");
        std::fs::create_dir_all(&domain_root).expect("a private domain root");
        let dead_node_owner = Command::new(std::env::current_exe().unwrap())
            .args([
                "python_helper_process_spawn_host::tests::\
                 a_helper_exit_reclaims_the_iceoryx2_nodes_it_left_whatever_ended_it",
                "--exact",
                "--test-threads=1",
            ])
            .env(
                DEAD_NODE_CHILD_DOMAIN_ROOT_ENVIRONMENT_VARIABLE,
                &domain_root,
            )
            .output()
            .expect("the test binary re-runs this test in a child process");
        // The signal and not merely a non-zero exit: a child that panicked
        // before it opened its node would also exit non-zero, and would leave
        // the domain empty — against which the zero below asserts nothing.
        assert_eq!(
            std::os::unix::process::ExitStatusExt::signal(&dead_node_owner.status),
            Some(libc::SIGKILL),
            "the child must die where it stood, holding its node: {}",
            String::from_utf8_lossy(&dead_node_owner.stderr),
        );

        // A host whose own helper has already left, closed the ordinary way.
        let mut host = spawn_host_for_test(None);
        host.iceoryx2_domain_root = Some(domain_root.clone());
        host.child = Some(
            Command::new("true")
                .spawn()
                .expect("a stand-in helper that is already done"),
        );

        host.take_the_helper_process_group_down();

        let left_for_somebody_else =
            streamlib::sdk::iceoryx2::reclaim_dead_iceoryx2_nodes_in_engine_owned_domain(
                &domain_root,
            )
            .expect("the domain can be swept");
        std::fs::remove_dir_all(&domain).ok();

        assert_eq!(
            left_for_somebody_else, 0,
            "the helper's exit left a dead node for somebody else to reclaim — the sweep runs \
             at every exit, not only at a crash the engine detected"
        );
    }

    #[test]
    fn every_path_onto_the_ladder_asks_the_helper_to_stop_and_tear_down_exactly_once() {
        // The plan owes a `teardown()` to any callback interrupted at
        // shutdown, `setup()` included — and a helper still importing then has
        // never reached the engine's `stop()` hook, which is where the commands
        // normally go out. So the ladder asks for itself, idempotently.
        //
        // Fail-without-fix: leave the send in `stop()` alone and the
        // registration path waits out both budgets for replies to commands
        // that were never sent, then warns that a teardown it never asked for
        // did not finish.
        let mut host = spawn_host_for_test(None);
        assert!(!host.shutdown_was_already_asked_of_this_helper);

        // With no bridge the ask short-circuits before its sends, so the flag
        // is the whole of what is observable here: that every route onto the
        // ladder goes through the ask, and that a second route does not repeat
        // it. That the commands then reach the wire is the rig scenario's, in
        // `test_helper_placement.py`.
        host.ask_the_helper_to_stop_and_tear_down();
        assert!(host.shutdown_was_already_asked_of_this_helper);

        host.shutdown_was_already_asked_of_this_helper = false;
        host.stop_the_helper_process_on_the_shutdown_ladder();
        assert!(
            host.shutdown_was_already_asked_of_this_helper,
            "the ladder walked without ever asking the helper to stop or tear down"
        );
    }

    #[test]
    fn the_two_shutdown_commands_are_the_ones_the_helper_answers() {
        // One pairing, in one place: the command the parent writes and the
        // reply tag the rung waits on come from the same value, so a rename of
        // either cannot leave the ladder waiting for a tag nobody sends.
        let [stop, teardown] =
            HelperProcessShutdownCommand::BOTH_IN_THE_ORDER_THE_LADDER_SENDS_THEM;

        assert_eq!((stop.command_tag(), stop.reply_tag()), ("stop", "stopped"));
        assert_eq!(
            (teardown.command_tag(), teardown.reply_tag()),
            ("teardown", "done")
        );
    }

    #[test]
    fn a_dead_helper_is_noticed_by_its_process_while_a_survivor_still_holds_its_socket() {
        // the plan's "a crash the engine detects by the process itself rather than
        // by its socket", asserted as the contrast it was written for: a worker
        // the helper forked inherits the escalate socket through the fork, so
        // the bridge's EOF never arrives and the engine used to read a dead
        // helper as a live one — still wired, still able to escalate.
        let mut command = Command::new("python3");
        command
            .arg("-c")
            .arg(A_HELPER_THAT_FORKS_A_WORKER_AND_LEAVES)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        detach_child_from_the_terminal_and_bind_its_lifetime_to_ours(&mut command);
        give_the_child_no_descriptor_beyond_stdio(&mut command);
        let mut escalate_transport =
            EscalateTransport::attach(&mut command).expect("an escalate socketpair");

        let child = command.spawn().expect("the stub helper to start");
        let helper_process_id = child.id();
        escalate_transport.release_child_end();
        let parent_end_of_the_escalate_socket = escalate_transport.into_parent_stream();

        assert!(
            a_helper_process_becomes_collectable_within(helper_process_id, Duration::from_secs(5)),
            "the stub helper never exited"
        );
        assert!(
            !a_descriptor_reports_end_of_file_within(
                parent_end_of_the_escalate_socket.as_raw_fd(),
                Duration::from_millis(500),
            ),
            "the socket reached EOF, so this asserts nothing the old signal did not already catch"
        );

        // SAFETY: the group is this test's own child's; the survivor is in it.
        unsafe { libc::killpg(helper_process_id as libc::pid_t, libc::SIGKILL) };
        let mut child = child;
        let _ = child.wait();
    }

    #[test]
    fn the_escalate_socket_survives_the_sweep_that_takes_every_other_descriptor() {
        // The sweep marks everything past stdio close-on-exec and
        // `EscalateTransport::attach` clears the flag again on the one
        // descriptor a helper is owed, so the order of the two `pre_exec`
        // registrations is the whole of whether a helper has a channel at all.
        let mut command = Command::new("python3");
        command
            .arg("-c")
            .arg(
                r#"
import os, sys
os.fstat(int(os.environ["STREAMLIB_ESCALATE_FD"]))
sys.exit(0)
"#,
            )
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        detach_child_from_the_terminal_and_bind_its_lifetime_to_ours(&mut command);
        give_the_child_no_descriptor_beyond_stdio(&mut command);
        let mut escalate_transport =
            EscalateTransport::attach(&mut command).expect("an escalate socketpair");

        let mut child = command.spawn().expect("the stub helper to start");
        escalate_transport.release_child_end();
        let exit_status = child.wait().expect("the stub helper to exit");

        assert!(
            exit_status.success(),
            "the helper could not stat the escalate socket it was handed: {exit_status}"
        );
    }

    /// Read a built command's environment back as pairs, so a test can assert
    /// what a child would inherit without starting one.
    fn environment_of(command: &Command) -> Vec<(String, String)> {
        command
            .get_envs()
            .filter_map(|(name, value)| {
                Some((
                    name.to_string_lossy().into_owned(),
                    value?.to_string_lossy().into_owned(),
                ))
            })
            .collect()
    }

    fn value_of<'a>(environment: &'a [(String, String)], name: &str) -> Option<&'a str> {
        environment
            .iter()
            .find(|(entry_name, _)| entry_name == name)
            .map(|(_, value)| value.as_str())
    }

    fn spawn_host_for_test(
        app_entry_directory: Option<PathBuf>,
    ) -> PythonHelperProcessSpawnHostProcessor {
        PythonHelperProcessSpawnHostProcessor {
            processor_class_import_path: "my_app.filters:BlurProcessor".to_string(),
            processor_display_name: "BlurProcessor".to_string(),
            processor_id: "Pblur".to_string(),
            processor_configuration: None,
            descriptor: ProcessorDescriptor::new(
                streamlib::sdk::descriptors::ProcessorClassShortName::new("BlurProcessor").unwrap(),
                streamlib::sdk::descriptors::ProcessorClassImportPath::new(
                    "my_app.filters:BlurProcessor",
                )
                .unwrap(),
                "a test double",
            ),
            child_execution_config: ExecutionConfig::new(ProcessExecution::Reactive),
            interpreter_path: PathBuf::from("/venv/bin/python"),
            app_entry_directory,
            child: None,
            iceoryx2_domain_root: None,
            child_standard_error_tail: None,
            bridge: None,
            child_is_gone: false,
            shutdown_was_already_asked_of_this_helper: false,
            link_wiring: OutOfProcessLinkWiringEnvelope::default(),
        }
    }

    /// A link wired into a helper that has died is refused, so the compile
    /// wiring it fails and the caller hears it; before setup the same call is
    /// a no-op, because the setup command carries the envelope.
    #[test]
    fn a_late_link_into_a_failed_helper_is_refused_and_one_before_setup_is_not() {
        let link_wiring = serde_json::json!({"link_id": "L-late", "name": "frames_from_upstream"});

        let mut failed = spawn_host_for_test(None);
        failed.child_is_gone = true;
        let refused = failed
            .wire_out_of_process_link(streamlib::sdk::error::PortDirection::Input, &link_wiring)
            .expect_err("a dead child can open no port");
        assert!(
            refused.to_string().contains("has failed"),
            "the refusal names the failure; got {refused}"
        );

        let mut not_yet_set_up = spawn_host_for_test(None);
        not_yet_set_up
            .wire_out_of_process_link(streamlib::sdk::error::PortDirection::Input, &link_wiring)
            .expect("before setup the envelope carries the link");
    }

    /// The child is an exec of the app's own interpreter running the helper
    /// module — never a fork, and never some other Python found on `PATH`.
    #[test]
    fn the_child_is_the_apps_own_interpreter_running_the_helper_module() {
        let command = spawn_host_for_test(None).build_helper_process_command(
            "Rtest",
            Path::new("/tmp/streamlib-1000/iox2"),
            None,
        );
        assert_eq!(command.get_program(), OsStr::new("/venv/bin/python"));
        let arguments: Vec<_> = command.get_args().collect();
        assert_eq!(arguments, ["-m", "streamlib._helper"]);
    }

    /// The class the child imports, and the identifiers it reports itself by,
    /// travel in the environment. `STREAMLIB_ENTRYPOINT` *is* the import path
    /// `rt.add` derived and refused an unimportable class by.
    #[test]
    fn the_child_is_told_which_class_to_import_and_who_it_is() {
        let command = spawn_host_for_test(None).build_helper_process_command(
            "Rtest",
            Path::new("/tmp/streamlib-1000/iox2"),
            None,
        );
        let environment = environment_of(&command);
        assert_eq!(
            value_of(&environment, "STREAMLIB_ENTRYPOINT"),
            Some("my_app.filters:BlurProcessor")
        );
        assert_eq!(
            value_of(&environment, "STREAMLIB_PROCESSOR_ID"),
            Some("Pblur")
        );
        assert_eq!(
            value_of(&environment, "STREAMLIB_RUNTIME_ID"),
            Some("Rtest")
        );
    }

    /// The child opens its iceoryx2 node in the domain its parent resolved, so
    /// the two always share one domain whatever the child's working directory.
    #[test]
    fn the_child_is_handed_the_parents_iceoryx2_domain_root() {
        let command = spawn_host_for_test(None).build_helper_process_command(
            "Rtest",
            Path::new("/tmp/streamlib-1000/iox2"),
            None,
        );
        let environment = environment_of(&command);
        assert_eq!(
            value_of(&environment, "STREAMLIB_ICEORYX2_DOMAIN_ROOT"),
            Some("/tmp/streamlib-1000/iox2")
        );
    }

    /// The child is handed the id of the engine this parent was compiled from,
    /// and refuses to start unless the engine it imports carries the same one.
    #[test]
    fn the_child_is_handed_the_engine_build_id_it_must_match() {
        let command = spawn_host_for_test(None).build_helper_process_command(
            "Rtest",
            Path::new("/tmp/streamlib-1000/iox2"),
            None,
        );
        let environment = environment_of(&command);
        assert_eq!(
            value_of(&environment, "STREAMLIB_ENGINE_BUILD_ID"),
            Some(ENGINE_BUILD_ID)
        );
    }

    /// A helper refuses its own start on raw standard error before its log
    /// channel exists, so the processor's refusal is the only place an
    /// operator reads why. A real child, so the pipe closes the way a helper's
    /// does, refused through the host's own path out of a setup that failed.
    ///
    /// Fail-without-fix: refuse without the tail and the refusal names the
    /// processor but not the two build ids.
    #[test]
    fn a_helper_that_died_while_setting_up_is_refused_naming_what_it_wrote_to_standard_error() {
        let mut child = Command::new("sh")
            .args([
                "-c",
                "printf 'some earlier line\\n[streamlib] this helper imported engine build A, \
                 its parent is build B\\n' >&2; exit 1",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .expect("sh starts");
        let mut host = spawn_host_for_test(None);
        host.child_standard_error_tail = Some(spawn_standard_error_reader_keeping_its_tail(
            child.stderr.take().expect("stderr is piped"),
            "Pblur",
        ));
        child.wait().expect("sh exits");

        let refusal = host
            .refuse_the_helper_process_that_died_while_setting_up()
            .to_string();

        assert!(host.child_is_gone);
        assert!(
            refusal
                .contains("[BlurProcessor] its helper process died before it finished setting up"),
            "{refusal}"
        );
        assert!(
            refusal.ends_with(
                "some earlier line\n[streamlib] this helper imported engine build A, its parent \
                 is build B"
            ),
            "{refusal}"
        );
    }

    /// A descendant still holding the pipe cannot stall the refusal: the wait
    /// gives up at its deadline with what had arrived.
    #[test]
    fn a_standard_error_left_open_bounds_the_wait_and_keeps_what_arrived() {
        let (mut still_open_writer, reader) =
            std::os::unix::net::UnixStream::pair().expect("socketpair");
        let tail = spawn_standard_error_reader_keeping_its_tail(reader, "Pblur");
        std::io::Write::write_all(&mut still_open_writer, b"said before exiting\n").unwrap();
        let recorded_by = Instant::now() + Duration::from_secs(10);
        while tail.text_once_closed_or_after(Duration::ZERO).is_empty() {
            assert!(Instant::now() < recorded_by, "the line was never recorded");
            std::thread::yield_now();
        }

        let (text_sender, text_receiver) = std::sync::mpsc::channel();
        let waiting_tail = tail.clone();
        std::thread::spawn(move || {
            let _ = text_sender
                .send(waiting_tail.text_once_closed_or_after(Duration::from_millis(200)));
        });
        let text = text_receiver
            .recv_timeout(Duration::from_secs(10))
            .expect("the wait on a pipe nobody closed returned at its deadline");

        assert_eq!(text, "said before exiting");
        drop(still_open_writer);
    }

    /// Only the end of a long standard error is kept, so a helper that wrote
    /// without bound cannot grow the parent's memory with it.
    #[test]
    fn only_the_last_bytes_of_a_long_standard_error_are_kept() {
        let tail = HelperProcessStandardErrorTail::default();
        tail.record(b"the earliest bytes, overwritten");
        tail.record(&vec![b'x'; STANDARD_ERROR_TAIL_BYTES]);
        tail.record(b"the reason");
        tail.mark_the_pipe_closed();

        let text = tail.text_once_closed_or_after(Duration::ZERO);

        assert_eq!(text.len(), STANDARD_ERROR_TAIL_BYTES);
        assert!(text.ends_with("xthe reason"));
    }

    /// A helper that died writing nothing is still refused by name, and says
    /// there was nothing to carry rather than ending on an empty line.
    #[test]
    fn a_helper_that_died_writing_nothing_is_refused_saying_so() {
        let refusal =
            refusal_of_a_helper_process_that_died_while_setting_up("BlurProcessor", "").to_string();
        assert!(
            refusal.ends_with(
                "[BlurProcessor] its helper process died before it finished setting up, and \
                 wrote nothing to its standard error"
            ),
            "{refusal}"
        );
    }

    /// The app's import root leads the child's `PYTHONPATH`, which is the only
    /// reason a processor module sitting beside the entry file is importable
    /// in a child launched from somewhere else entirely.
    #[test]
    fn the_apps_import_root_leads_the_childs_python_path() {
        let app_entry_directory = std::env::temp_dir();
        let command = spawn_host_for_test(Some(app_entry_directory.clone()))
            .build_helper_process_command("Rtest", Path::new("/tmp/streamlib-1000/iox2"), None);
        let environment = environment_of(&command);
        let python_path = value_of(&environment, "PYTHONPATH").expect("PYTHONPATH is set");
        assert_eq!(
            python_path.split(':').next(),
            Some(app_entry_directory.to_string_lossy().as_ref()),
            "the app's own modules must resolve before anything inherited"
        );
    }

    /// An inherited `PYTHONHOME` points at whatever laid out the *parent's*
    /// install; the child's interpreter was found by absolute path, so keeping
    /// it would only send the child looking for the wrong standard library.
    #[test]
    fn an_inherited_python_home_is_not_passed_to_the_child() {
        let command = spawn_host_for_test(None).build_helper_process_command(
            "Rtest",
            Path::new("/tmp/streamlib-1000/iox2"),
            None,
        );
        let cleared: Vec<_> = command
            .get_envs()
            .filter(|(name, value)| *name == OsStr::new("PYTHONHOME") && value.is_none())
            .collect();
        assert_eq!(cleared.len(), 1, "PYTHONHOME must be explicitly removed");
    }

    /// `sys.path[0]`, not `sys.argv[0]`: the launcher restores its own argv
    /// before the `Runtime` is built, so an argv-derived root is the wheel's
    /// own package directory and the child cannot import the app at all.
    #[test]
    fn an_empty_import_root_is_no_root_rather_than_the_filesystem_root() {
        assert!(app_import_root_directory("").is_none());
        assert!(app_import_root_directory("/definitely/not/a/real/path").is_none());
        let real = std::env::temp_dir();
        assert_eq!(
            app_import_root_directory(&real.to_string_lossy()),
            real.canonicalize().ok()
        );
    }
}
