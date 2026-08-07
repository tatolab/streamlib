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

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use pyo3::prelude::*;
use streamlib::sdk::context::{RuntimeContextFullAccess, RuntimeContextLimitedAccess};
use streamlib::sdk::descriptors::ProcessorDescriptor;
use streamlib::sdk::error::{Error, Result};
use streamlib::sdk::execution::{ExecutionConfig, ProcessExecution};
use streamlib::sdk::graph::ProcessorNode;
use streamlib::sdk::helper_process_transport::{
    EscalateTransport, PROTOCOL_VERSION_ENV, STREAMLIB_SUBPROCESS_PROTOCOL_VERSION,
    SubprocessBridge, spawn_fd_line_reader, validate_subprocess_protocol,
};
use streamlib::sdk::processors::{DynGeneratedProcessor, OutOfProcessLinkWiringEnvelope};

/// The module CPython is launched with in a helper process.
const HELPER_PROCESS_MODULE: &str = "streamlib._helper";

/// How long the child has to import the user's class, open its ports, run
/// `setup` and report ready before this host gives up and kills it.
///
/// Generous because a cold child imports the whole wheel — but bounded, so a
/// class that blocks at import time fails the graph instead of hanging it.
const REGISTRATION_DEADLINE: Duration = Duration::from_secs(60);

/// How long a child gets to exit on its own after teardown before it is killed.
const TEARDOWN_EXIT_DEADLINE: Duration = Duration::from_secs(5);

/// How long a child gets to answer a lifecycle command before the parent stops
/// waiting on it.
///
/// This bounds the engine's own thread, not the child's work: the callbacks
/// behind these commands are expected to return promptly, and a child that
/// needs longer has already broken the contract.
const REPLY_DEADLINE: Duration = Duration::from_secs(5);

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
    bridge: Option<SubprocessBridge>,
    /// Set once the child stops answering. The pipeline keeps running and the
    /// graph shows this processor in error; the frame in flight is lost, and
    /// is never silently replayed.
    child_is_gone: bool,
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
            .env("STREAMLIB_ENTRYPOINT", &self.processor_class_import_path)
            .env("STREAMLIB_PROCESSOR_ID", &self.processor_id)
            .env("STREAMLIB_RUNTIME_ID", runtime_id)
            .env(
                PROTOCOL_VERSION_ENV,
                STREAMLIB_SUBPROCESS_PROTOCOL_VERSION.to_string(),
            );
        if let Some(surface_socket_path) = surface_socket_path {
            command.env("STREAMLIB_SURFACE_SOCKET", surface_socket_path);
        }
        detach_child_from_the_terminal_and_bind_its_lifetime_to_ours(&mut command);
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
        let reply_tag = reply.get("rpc").and_then(|rpc| rpc.as_str()).unwrap_or("");
        if reply_tag != expected_reply_tag {
            let reported = reply
                .get("error")
                .and_then(|error| error.as_str())
                .unwrap_or("it reported no reason");
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
        let reply = loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                self.kill_child();
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
                    self.child_is_gone = true;
                    return Err(Error::Runtime(format!(
                        "[{}] its helper process died before it finished setting up",
                        self.processor_display_name
                    )));
                }
            }
        };

        match reply.get("rpc").and_then(|rpc| rpc.as_str()) {
            Some("ready") => {
                validate_subprocess_protocol(
                    reply
                        .get("protocol_version")
                        .and_then(|version| version.as_u64())
                        .and_then(|version| u32::try_from(version).ok()),
                    &self.processor_display_name,
                )?;
                Ok(())
            }
            _ => {
                let reported = reply
                    .get("error")
                    .and_then(|error| error.as_str())
                    .unwrap_or("it reported no reason");
                Err(Error::Runtime(format!(
                    "[{}] could not set itself up in its helper process:\n{reported}",
                    self.processor_display_name
                )))
            }
        }
    }

    fn kill_child(&mut self) {
        self.child_is_gone = true;
        self.bridge.take();
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }

    /// Wait for the child to exit on its own, killing it once the deadline
    /// passes. Either way it is reaped, so `rt.run()` leaves no survivors.
    fn reap_child(&mut self) {
        let Some(mut child) = self.child.take() else {
            return;
        };
        let deadline = Instant::now() + TEARDOWN_EXIT_DEADLINE;
        loop {
            match child.try_wait() {
                Ok(Some(exit_status)) => {
                    tracing::debug!(
                        "[{}] helper process exited: {exit_status}",
                        self.processor_display_name
                    );
                    return;
                }
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(20));
                }
                _ => {
                    tracing::warn!(
                        "[{}] helper process did not exit on its own; killing it",
                        self.processor_display_name
                    );
                    let _ = child.kill();
                    let _ = child.wait();
                    return;
                }
            }
        }
    }
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

impl DynGeneratedProcessor for PythonHelperProcessSpawnHostProcessor {
    fn __generated_setup(&mut self, ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        #[cfg(target_os = "linux")]
        let surface_socket_path = Some(ctx.surface_socket_path());
        #[cfg(not(target_os = "linux"))]
        let surface_socket_path: Option<&Path> = None;

        let mut command = self.build_helper_process_command(&ctx.runtime_id(), surface_socket_path);
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

        // fd1/fd2 carry anything that bypasses `streamlib.log` — a raw
        // `os.write`, a C extension's `printf`, an interpreter-level fatal —
        // and each line becomes an `intercepted` record in the unified JSONL.
        if let Some(child_stdout) = child.stdout.take() {
            spawn_fd_line_reader(child_stdout, "py-stdout", "fd1", &self.processor_id);
        }
        if let Some(child_stderr) = child.stderr.take() {
            spawn_fd_line_reader(child_stderr, "py-stderr", "fd2", &self.processor_id);
        }

        self.child = Some(child);
        self.bridge = Some(SubprocessBridge::new(
            escalate_transport.into_parent_stream(),
            ctx.gpu_limited_access().clone(),
            self.processor_id.clone(),
        )?);

        self.send_to_child(&serde_json::json!({
            "cmd": "setup",
            "capability": "full",
            "config": self
                .processor_configuration
                .clone()
                .unwrap_or(serde_json::Value::Null),
            "processor_id": self.processor_id,
            "ports": self.link_wiring.as_setup_command_ports(),
        }))?;
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
        self.exchange_with_child_expecting(
            &serde_json::json!({"cmd": "stop", "capability": "full"}),
            "stop",
            "stopped",
        );
        Ok(())
    }

    fn __generated_teardown(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        self.exchange_with_child_expecting(
            &serde_json::json!({"cmd": "teardown", "capability": "full"}),
            "teardown",
            "done",
        );
        // Dropping the bridge closes this end, so a child still in its loop
        // sees EOF and leaves it even if the teardown command never landed.
        self.bridge.take();
        self.reap_child();
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

    fn has_failed_unrecoverably(&self) -> bool {
        // The bridge, not just this host's own flag: between `run` and
        // teardown the parent sends nothing, so a child that dies mid-run is
        // never noticed by a failed exchange. What does notice is the bridge's
        // reader thread, which sees EOF on the socket the moment the child's
        // last fd closes — with no living process, that is the only signal
        // there is.
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
        let reply_tag = reply.get("rpc").and_then(|rpc| rpc.as_str()).unwrap_or("");
        if reply_tag != "ok" {
            let reported = reply
                .get("error")
                .and_then(|error| error.as_str())
                .unwrap_or("it reported no reason");
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
            self.kill_child();
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
        bridge: None,
        child_is_gone: false,
        link_wiring: OutOfProcessLinkWiringEnvelope::default(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;

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
                streamlib::sdk::descriptors::SchemaIdent::new(
                    streamlib::sdk::descriptors::Org::new("app").unwrap(),
                    streamlib::sdk::descriptors::Package::new("local").unwrap(),
                    streamlib::sdk::descriptors::TypeName::new("BlurProcessor").unwrap(),
                    streamlib::sdk::descriptors::SemVer::new(0, 0, 0),
                ),
                "a test double",
            ),
            child_execution_config: ExecutionConfig::new(ProcessExecution::Reactive),
            interpreter_path: PathBuf::from("/venv/bin/python"),
            app_entry_directory,
            child: None,
            bridge: None,
            child_is_gone: false,
            link_wiring: OutOfProcessLinkWiringEnvelope::default(),
        }
    }

    /// The child is an exec of the app's own interpreter running the helper
    /// module — never a fork, and never some other Python found on `PATH`.
    #[test]
    fn the_child_is_the_apps_own_interpreter_running_the_helper_module() {
        let command = spawn_host_for_test(None).build_helper_process_command("Rtest", None);
        assert_eq!(command.get_program(), OsStr::new("/venv/bin/python"));
        let arguments: Vec<_> = command.get_args().collect();
        assert_eq!(arguments, ["-m", "streamlib._helper"]);
    }

    /// The class the child imports, and the identifiers it reports itself by,
    /// travel in the environment. `STREAMLIB_ENTRYPOINT` *is* the import path
    /// `rt.add` derived and refused an unimportable class by.
    #[test]
    fn the_child_is_told_which_class_to_import_and_who_it_is() {
        let command = spawn_host_for_test(None).build_helper_process_command("Rtest", None);
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

    /// The app's import root leads the child's `PYTHONPATH`, which is the only
    /// reason a processor module sitting beside the entry file is importable
    /// in a child launched from somewhere else entirely.
    #[test]
    fn the_apps_import_root_leads_the_childs_python_path() {
        let app_entry_directory = std::env::temp_dir();
        let command = spawn_host_for_test(Some(app_entry_directory.clone()))
            .build_helper_process_command("Rtest", None);
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
        let command = spawn_host_for_test(None).build_helper_process_command("Rtest", None);
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
