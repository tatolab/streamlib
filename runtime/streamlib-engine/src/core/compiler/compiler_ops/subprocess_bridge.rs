// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Length-prefixed JSON escalate-IPC bridge shared by the Python and Deno
//! subprocess host processors.
//!
//! Frames travel over a dedicated [`UnixStream`] pair created by
//! [`EscalateTransport::attach`] before spawn — not over the subprocess's
//! stdin/stdout. The parent keeps one half of the socketpair and the
//! child inherits the other via `STREAMLIB_ESCALATE_FD`, freeing fd1/fd2
//! to be captured as intercepted log pipes by the host.
//!
//! Three roles travel over the same socket:
//! 1. Lifecycle RPC (`setup`, `run`, `stop`, `teardown`, `on_pause`,
//!    `on_resume`, …) — initiated by the host, the subprocess replies with
//!    `rpc: "ready" | "stopped" | "ok" | "done" | "error"`.
//! 2. Escalate-on-behalf (`rpc: "escalate_request"`) — initiated by the
//!    subprocess, the host replies with `rpc: "escalate_response"`.
//! 3. A link's wire answer ([`OUT_OF_PROCESS_LINK_WIRED_RPC`],
//!    [`OUT_OF_PROCESS_LINK_WIRE_FAILED_RPC`]) — the subprocess's answer to a
//!    `wire_link` the host sent after setup, naming the link it is about.
//!
//! A dedicated reader thread (`br-…`) owns the parent-side read half and
//! demultiplexes incoming messages: escalate requests are dispatched
//! inline through [`subprocess_escalate::process_bridge_message`], a link's
//! wire answer lands on that link's own cell, and anything else is forwarded
//! to the main thread over an mpsc channel for the lifecycle RPC to consume.
//! The third role has its own tags for exactly that reason: an answer routed
//! to the lifecycle queue would be read as the reply to whatever command the
//! host sends next. Writes in both directions serialize
//! through a shared `Arc<Mutex<BufWriter<UnixStream>>>` so the main
//! thread and the reader thread can't interleave halves of a
//! length-prefixed frame.

use std::io::{BufReader, BufWriter, Read, Write};
use std::os::unix::io::{AsRawFd, RawFd};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::process::Command;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crate::core::context::GpuContextLimitedAccess;
use crate::core::error::{Error, Result};
use crate::core::processors::{
    LinksAwaitingTheirOutOfProcessWireReply, OutOfProcessLinkWireOutcome, OutOfProcessLinkWireReply,
};

use super::subprocess_escalate::{
    EscalateHandleRegistry, process_bridge_message,
    release_surface_share_and_texture_cache_for_handle,
};

/// Env var advertising the inherited child-end fd number of the escalate
/// socketpair. The subprocess opens this fd as a duplex UNIX socket and
/// uses it as the framed-IPC transport.
pub(crate) const ESCALATE_FD_ENV: &str = "STREAMLIB_ESCALATE_FD";

/// This engine's build id: its crate version, the git sha it was built from
/// (`unknown` where the build had no checkout) and a nonce minted each time the
/// build script ran, so two builds of one sha still differ.
pub const ENGINE_BUILD_ID: &str = env!("STREAMLIB_ENGINE_BUILD_ID_FROM_BUILD_SCRIPT");

/// Env var carrying the parent's [`ENGINE_BUILD_ID`] to a helper process, which
/// refuses to start unless the engine it imported was compiled with the same id.
///
/// Parent and helper import one wheel, so the ids differ only when the helper
/// imported another build — a stale `streamlib` earlier on its `sys.path`, or
/// an engine built against another iceoryx2 — which would otherwise surface as
/// every service open failing on a corrupted service.
pub const ENGINE_BUILD_ID_ENVIRONMENT_VARIABLE: &str = "STREAMLIB_ENGINE_BUILD_ID";

/// Socketpair-backed escalate IPC transport. The parent holds one half
/// and the subprocess inherits the other via [`ESCALATE_FD_ENV`].
pub struct EscalateTransport {
    parent_end: UnixStream,
    /// Kept alive so the child fd stays open across `Command::spawn`. The
    /// caller drops this after spawn so only the subprocess holds the
    /// child end.
    child_end: Option<UnixStream>,
}

impl EscalateTransport {
    /// Create a socketpair, register `pre_exec` on `command` to clear
    /// `FD_CLOEXEC` on the child-end fd, and set [`ESCALATE_FD_ENV`] on
    /// the command's environment.
    ///
    /// After `command.spawn()`, call [`Self::release_child_end`] so only
    /// the subprocess retains the child-side fd.
    pub fn attach(command: &mut Command) -> Result<Self> {
        let (parent_end, child_end) = UnixStream::pair()
            .map_err(|e| Error::Runtime(format!("failed to create escalate socketpair: {e}")))?;

        let child_fd: RawFd = child_end.as_raw_fd();

        // Clear FD_CLOEXEC on the child-end fd between fork and exec so
        // the execed subprocess inherits it. `fcntl` is async-signal-safe
        // so it's legal to call from `pre_exec`.
        unsafe {
            command.pre_exec(move || {
                let flags = libc::fcntl(child_fd, libc::F_GETFD);
                if flags < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                let rc = libc::fcntl(child_fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC);
                if rc < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }

        command.env(ESCALATE_FD_ENV, child_fd.to_string());

        Ok(Self {
            parent_end,
            child_end: Some(child_end),
        })
    }

    /// Drop the parent's reference to the child-end fd. Call this after
    /// `command.spawn()` succeeds so only the subprocess keeps it open.
    pub fn release_child_end(&mut self) {
        self.child_end.take();
    }

    /// Consume the transport and return the parent-side [`UnixStream`].
    pub fn into_parent_stream(mut self) -> UnixStream {
        self.child_end.take();
        self.parent_end
    }
}

/// The lifecycle command whose hook may mint a processor-owned window. A
/// window is a setup-phase resource request; `docs/plan/ARCHITECTURE.md`
/// §Media I/O has it "requested in `setup()` … never minted
/// mid-`process()`".
///
/// Named here because the escalate dispatch reads it and the spawn host that
/// sends it lives in another crate: a bare literal on each side would let a
/// rename refuse every window silently.
pub const SETUP_LIFECYCLE_COMMAND_TO_HELPER_PROCESS: &str = "setup";

/// A shutdown command the parent sends a helper, paired with the reply tag the
/// helper answers it with.
///
/// The pairing lives beside [`SETUP_LIFECYCLE_COMMAND_TO_HELPER_PROCESS`] and
/// the protocol this module's doc enumerates, so the two halves of one exchange
/// cannot drift apart across the crate boundary the spawn host sits on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HelperProcessShutdownCommand {
    /// Leave the execution loop and run the processor's `stop()`.
    Stop,
    /// Run the processor's `teardown()` and leave.
    Teardown,
}

impl HelperProcessShutdownCommand {
    /// Both commands, in the order the shutdown ladder sends them.
    ///
    /// `docs/plan/ARCHITECTURE.md` §Processor model, the `[shutdown-ladder]`
    /// entry: "`stop` and `teardown` are sent together".
    pub const BOTH_IN_THE_ORDER_THE_LADDER_SENDS_THEM: [Self; 2] = [Self::Stop, Self::Teardown];

    /// What the parent writes as the frame's `cmd`.
    pub fn command_tag(self) -> &'static str {
        match self {
            Self::Stop => "stop",
            Self::Teardown => "teardown",
        }
    }

    /// What the helper writes as the answering frame's `rpc`.
    pub fn reply_tag(self) -> &'static str {
        match self {
            Self::Stop => "stopped",
            Self::Teardown => "done",
        }
    }
}

/// The rpc tag a subprocess answers a late `wire_link` with once it has opened
/// its own port for the link. Carries `link_id`.
pub const OUT_OF_PROCESS_LINK_WIRED_RPC: &str = "link_wired";

/// The rpc tag a subprocess answers a late `wire_link` with when it could not
/// open its port. Carries `link_id` and `reason`.
pub const OUT_OF_PROCESS_LINK_WIRE_FAILED_RPC: &str = "link_wire_failed";

/// Where one frame arriving from the subprocess belongs.
///
/// Classified on the `rpc` tag alone, never on what a handler makes of the
/// frame: the routing decision is the wire contract's, and a frame that fell
/// through to the lifecycle queue by accident is read as the answer to the next
/// command the host sends.
#[derive(Debug, PartialEq, Eq)]
enum IncomingSubprocessFrame {
    /// An escalate request, dispatched inline on the reader thread.
    EscalateRequest,
    /// One link's answer to a `wire_link` the host sent after setup.
    LinkWireAnswer {
        link_id: String,
        outcome: OutOfProcessLinkWireOutcome,
    },
    /// A link's answer that names no link, so nothing can be routed to.
    LinkWireAnswerNamingNoLink,
    /// The answer to a lifecycle command, forwarded to the main thread.
    LifecycleReply,
}

fn classify_an_incoming_subprocess_frame(msg: &serde_json::Value) -> IncomingSubprocessFrame {
    let rpc = msg.get("rpc").and_then(|rpc| rpc.as_str());
    if rpc == Some(super::subprocess_escalate::ESCALATE_REQUEST_RPC) {
        return IncomingSubprocessFrame::EscalateRequest;
    }
    let outcome = match rpc {
        Some(OUT_OF_PROCESS_LINK_WIRED_RPC) => OutOfProcessLinkWireOutcome::OpenedByTheFarSide,
        Some(OUT_OF_PROCESS_LINK_WIRE_FAILED_RPC) => {
            OutOfProcessLinkWireOutcome::RefusedByTheFarSide {
                reason: msg
                    .get("reason")
                    .and_then(|reason| reason.as_str())
                    .unwrap_or("it did not say why")
                    .to_string(),
            }
        }
        _ => return IncomingSubprocessFrame::LifecycleReply,
    };
    match msg.get("link_id").and_then(|link_id| link_id.as_str()) {
        Some(link_id) => IncomingSubprocessFrame::LinkWireAnswer {
            link_id: link_id.to_string(),
            outcome,
        },
        None => IncomingSubprocessFrame::LinkWireAnswerNamingNoLink,
    }
}

/// Shared writer handle. The host's lifecycle path and the reader
/// thread's escalate-response path both write through this mutex.
type SharedWriter = Arc<Mutex<BufWriter<UnixStream>>>;

/// Bridge for one subprocess. Drop the value to tear the reader thread
/// down cleanly (shutdown the parent-side socket read half; reader
/// thread exits on EOF).
pub struct SubprocessBridge {
    processor_id: String,
    writer: SharedWriter,
    lifecycle_rx: Receiver<serde_json::Value>,
    registry: Arc<EscalateHandleRegistry>,
    /// Every link handed to this subprocess after its setup and not yet
    /// answered for. Shared with the reader thread, which is where the answers
    /// arrive.
    links_awaiting_their_wire_reply: Arc<LinksAwaitingTheirOutOfProcessWireReply>,
    /// Held for teardown: the drop path evicts what the registry's acquires
    /// entered into the parent's texture cache, which needs the same
    /// capability the reader thread dispatches against.
    sandbox: GpuContextLimitedAccess,
    reader: Option<JoinHandle<()>>,
    dead: Arc<Mutex<bool>>,
}

impl SubprocessBridge {
    /// Wrap a socketpair parent end and spawn the reader thread.
    ///
    /// `sandbox` is cloned into the reader thread so escalate requests
    /// can be dispatched without blocking the main thread. `processor_id`
    /// is used for thread naming and tracing.
    pub fn new(
        stream: UnixStream,
        sandbox: GpuContextLimitedAccess,
        processor_id: String,
    ) -> Result<Self> {
        let read_half = stream.try_clone().map_err(|e| {
            Error::Runtime(format!(
                "failed to clone escalate socketpair for reader: {e}"
            ))
        })?;
        let writer: SharedWriter = Arc::new(Mutex::new(BufWriter::new(stream)));
        let registry = EscalateHandleRegistry::new();
        let (tx, rx) = mpsc::channel();
        let dead = Arc::new(Mutex::new(false));

        let links_awaiting_their_wire_reply =
            Arc::new(LinksAwaitingTheirOutOfProcessWireReply::default());

        let thread_name = thread_name(&processor_id);
        let reader_writer = Arc::clone(&writer);
        let reader_registry = Arc::clone(&registry);
        let reader_dead = Arc::clone(&dead);
        let reader_processor_id = processor_id.clone();
        let reader_links_awaiting = Arc::clone(&links_awaiting_their_wire_reply);
        let teardown_sandbox = sandbox.clone();

        let reader = thread::Builder::new()
            .name(thread_name)
            .spawn(move || {
                reader_loop(
                    BufReader::new(read_half),
                    reader_writer,
                    sandbox,
                    reader_registry,
                    tx,
                    reader_dead,
                    reader_processor_id,
                    reader_links_awaiting,
                );
            })
            .expect("failed to spawn bridge reader thread");

        Ok(Self {
            processor_id,
            writer,
            lifecycle_rx: rx,
            registry,
            links_awaiting_their_wire_reply,
            sandbox: teardown_sandbox,
            reader: Some(reader),
            dead,
        })
    }

    /// Wait on one link's answer, so the reader thread can route it when it
    /// arrives.
    ///
    /// Called by the host as it sends the `wire_link`, before the answer can
    /// come back: registering after the send would race the reader.
    pub fn await_the_subprocesss_wire_answer_for_link(
        &self,
        link_id: String,
        reply: Arc<OutOfProcessLinkWireReply>,
    ) {
        self.links_awaiting_their_wire_reply
            .await_an_answer_for_link(link_id, reply);
    }

    /// Stop waiting on a link that is being disconnected before its answer
    /// arrived, so a dead subprocess does not refuse a link the graph no
    /// longer has.
    pub fn stop_awaiting_the_subprocesss_wire_answer_for_link(&self, link_id: &str) {
        self.links_awaiting_their_wire_reply
            .stop_awaiting_an_answer_for_link(link_id);
    }

    /// Write a length-prefixed JSON message to the subprocess.
    pub fn send(&self, msg: &serde_json::Value) -> Result<()> {
        if self.is_dead() {
            return Err(Error::Runtime(format!(
                "[{}] bridge marked dead, cannot send",
                self.processor_id
            )));
        }
        // The lifecycle command is the engine's only reading of which hook the
        // child is inside, and this is the one seam every command crosses.
        // Setup-phase-only escalate ops (minting a processor-owned window)
        // refuse on it, dispatched from the reader thread while the hook that
        // is allowed to ask is still running.
        if let Some(lifecycle_command) = msg.get("cmd").and_then(|c| c.as_str()) {
            self.registry
                .note_lifecycle_command_sent_to_the_helper_process(lifecycle_command);
        }
        let mut writer = self
            .writer
            .lock()
            .map_err(|_| Error::Runtime("subprocess writer mutex poisoned".to_string()))?;
        write_frame(&mut *writer, msg).map_err(|e| {
            self.mark_dead();
            e
        })
    }

    /// Block until the next lifecycle-tagged message arrives.
    pub fn recv_lifecycle(&self) -> Result<serde_json::Value> {
        self.lifecycle_rx.recv().map_err(|_| {
            self.mark_dead();
            Error::Runtime(format!(
                "[{}] subprocess escalate socket closed before reply",
                self.processor_id
            ))
        })
    }

    /// Block up to `timeout` for the next lifecycle-tagged message.
    pub fn recv_lifecycle_timeout(
        &self,
        timeout: Duration,
    ) -> std::result::Result<serde_json::Value, RecvTimeoutError> {
        self.lifecycle_rx.recv_timeout(timeout)
    }

    /// Mark the bridge dead; subsequent sends return immediately.
    ///
    /// Every link this subprocess was still to answer for is refused here: a
    /// link whose far side died with the answer outstanding never reads
    /// `wired`, and a caller that asked `graph` would otherwise wait on an
    /// answer nothing can send.
    pub fn mark_dead(&self) {
        if let Ok(mut dead) = self.dead.lock() {
            *dead = true;
        }
        self.links_awaiting_their_wire_reply
            .refuse_every_link_still_awaiting_an_answer(&format!(
                "the helper process hosting '{}' is gone, so it never opened its port for this \
                 link",
                self.processor_id
            ));
    }

    pub fn is_dead(&self) -> bool {
        self.dead.lock().map(|g| *g).unwrap_or(true)
    }

    /// Count of escalate-acquired handles the host still holds. Used by
    /// teardown logging and tests.
    pub(crate) fn registry(&self) -> &Arc<EscalateHandleRegistry> {
        &self.registry
    }
}

impl Drop for SubprocessBridge {
    fn drop(&mut self) {
        self.mark_dead();
        // Shut the socket down before draining: until the reader thread sees
        // EOF it keeps dispatching escalate requests, and an acquire landing
        // after the drain would strand its cache entry — the very leak the
        // drain exists to close. A request already executing when the
        // shutdown lands can still slip through; closing that too would mean
        // joining the reader, which this path deliberately never blocks on.
        // The OS reaps the thread on process exit.
        if let Ok(writer) = self.writer.lock() {
            let _ = writer.get_ref().shutdown(std::net::Shutdown::Both);
        }
        // Windows first: each present thread resolves surface ids against the
        // same capability the handle release below evicts from, and dropping
        // one closes its window and joins its thread. A helper that never
        // called `close_processor_owned_window` — or crashed — releases its
        // windows here, which is what makes teardown the backstop the plan
        // says it is.
        for (window_id, present_loop) in self.registry.drain_processor_owned_windows() {
            tracing::debug!(
                "[{}] closing processor-owned window '{}' at teardown",
                self.processor_id,
                window_id
            );
            // Closed explicitly rather than by dropping the `Arc`: a request
            // still in flight on the reader thread can hold the last
            // reference, and teardown must close the window rather than hand
            // that decision to whoever lets go last. The close is bounded and
            // detaches, so this path still never blocks indefinitely.
            present_loop.close_the_window_and_join_its_present_thread();
        }
        // Run the release path's kind-specific cleanup for everything the
        // helper never released — a crashed child must not strand cache
        // entries in a GpuContext that outlives every respawn, nor
        // surface-share registrations and their fd dups: the host's own
        // connection registered those on the helper's behalf, so the
        // service's disconnect watchdog rightly never reclaims them.
        for (handle_id, removed_handle) in self.registry.drain_handles() {
            release_surface_share_and_texture_cache_for_handle(
                &self.sandbox,
                &handle_id,
                &removed_handle,
            );
        }
        if let Some(reader) = self.reader.take() {
            drop(reader);
        }
    }
}

/// Reader loop: drain the parent-side socket, dispatch escalate traffic,
/// forward lifecycle responses to `lifecycle_tx`.
fn reader_loop(
    mut reader: BufReader<UnixStream>,
    writer: SharedWriter,
    sandbox: GpuContextLimitedAccess,
    registry: Arc<EscalateHandleRegistry>,
    lifecycle_tx: mpsc::Sender<serde_json::Value>,
    dead: Arc<Mutex<bool>>,
    processor_id: String,
    links_awaiting_their_wire_reply: Arc<LinksAwaitingTheirOutOfProcessWireReply>,
) {
    loop {
        let msg = match read_frame(&mut reader) {
            Ok(v) => v,
            Err(e) => {
                tracing::debug!("[{}] bridge reader exiting: {}", processor_id, e);
                if let Ok(mut dead) = dead.lock() {
                    *dead = true;
                }
                break;
            }
        };

        // Classify the frame on the rpc tag, not the handler's reply
        // shape: fire-and-forget escalate ops (e.g. log) consume the
        // message but produce no response, so a `None` from
        // `process_bridge_message` cannot be used as the "this wasn't
        // an escalate request" signal — that would silently re-route
        // every log message to the lifecycle queue and trip the
        // setup/teardown waiters.
        let frame_route = classify_an_incoming_subprocess_frame(&msg);

        if let IncomingSubprocessFrame::LinkWireAnswer { link_id, outcome } = &frame_route {
            if !links_awaiting_their_wire_reply
                .note_the_far_sides_answer_for_link(link_id, outcome.clone())
            {
                tracing::warn!(
                    "[{}] its helper process answered for link '{}', which no link is waiting on",
                    processor_id,
                    link_id
                );
            }
            continue;
        }

        if frame_route == IncomingSubprocessFrame::LinkWireAnswerNamingNoLink {
            tracing::warn!(
                "[{}] its helper process answered a wire with no link named, so the link it \
                 meant stays unanswered",
                processor_id
            );
            continue;
        }

        if frame_route == IncomingSubprocessFrame::EscalateRequest {
            if let Some(response) = process_bridge_message(&sandbox, &registry, &msg) {
                // Escalate request handled inline. Write response with the
                // shared writer lock.
                let send_result: Result<()> = {
                    let mut writer = match writer.lock() {
                        Ok(g) => g,
                        Err(_) => {
                            tracing::warn!(
                                "[{}] bridge reader saw poisoned writer mutex",
                                processor_id
                            );
                            break;
                        }
                    };
                    write_frame(&mut *writer, &response)
                };
                if let Err(e) = send_result {
                    tracing::warn!(
                        "[{}] bridge reader failed to write escalate response: {}",
                        processor_id,
                        e
                    );
                    if let Ok(mut dead) = dead.lock() {
                        *dead = true;
                    }
                    break;
                }
            }
            // Fire-and-forget ops (log) leave nothing to write. Either way,
            // never forward escalate traffic to the lifecycle channel.
            continue;
        }

        // Lifecycle response — forward to main thread. Send failure
        // means the receiver is gone (host dropped), exit cleanly.
        if lifecycle_tx.send(msg).is_err() {
            tracing::debug!(
                "[{}] bridge reader exiting: lifecycle channel dropped",
                processor_id
            );
            break;
        }
    }

    // Every way out of the loop above is this reader giving up on the
    // subprocess, so the refusal sits here rather than on each `break`: a link
    // still waiting on an answer is refused once, whichever way the reader
    // stopped, and never reads `wired`.
    links_awaiting_their_wire_reply.refuse_every_link_still_awaiting_an_answer(&format!(
        "the helper process hosting '{processor_id}' stopped answering before it opened its port \
         for this link"
    ));
}

/// Per-line reader that tags each non-empty line with
/// `intercepted=true, channel=<channel>, source=python|deno` and emits
/// it as a `tracing::warn!` event. Used by the Python and Deno spawn
/// paths on the subprocess's fd1 (stdout) and fd2 (stderr). `channel`
/// must be `"fd1"` or `"fd2"`; the source and tracing target are
/// inferred from `thread_prefix` (`"py-…"` → python, `"dn-…"` → deno).
///
/// Captures the caller's current [`tracing::Dispatch`] and installs it
/// as the reader thread's default, so events route through whatever
/// subscriber the owning runtime installed (global for production,
/// thread-local for `init_for_tests`).
pub fn spawn_fd_line_reader<R>(
    reader: R,
    thread_prefix: &str,
    channel: &'static str,
    processor_id: &str,
) -> Option<JoinHandle<()>>
where
    R: Read + Send + 'static,
{
    let proc_id = processor_id.to_string();
    let short = &proc_id[..8.min(proc_id.len())];
    let name = format!("{}-{}", thread_prefix, short);
    let (source, target): (&'static str, &'static str) = if thread_prefix.starts_with("py") {
        ("python", "streamlib::polyglot::python")
    } else {
        ("deno", "streamlib::polyglot::deno")
    };
    let dispatch = tracing::dispatcher::get_default(|d| d.clone());

    thread::Builder::new()
        .name(name)
        .spawn(move || {
            use std::io::BufRead;
            tracing::dispatcher::with_default(&dispatch, || {
                let reader = BufReader::new(reader);
                for line in reader.lines() {
                    match line {
                        Ok(text) if !text.is_empty() => {
                            emit_intercepted_line(target, channel, source, &proc_id, &text);
                        }
                        Err(_) => break,
                        _ => {}
                    }
                }
            });
        })
        .ok()
}

fn emit_intercepted_line(
    target: &'static str,
    channel: &'static str,
    source: &'static str,
    processor_id: &str,
    text: &str,
) {
    // `tracing` macros require a literal target, so dispatch on the two
    // known targets here. Fields are identical across both call sites.
    match target {
        "streamlib::polyglot::python" => tracing::warn!(
            target: "streamlib::polyglot::python",
            intercepted = true,
            channel = channel,
            source = source,
            processor_id = %processor_id,
            "{}",
            text
        ),
        _ => tracing::warn!(
            target: "streamlib::polyglot::deno",
            intercepted = true,
            channel = channel,
            source = source,
            processor_id = %processor_id,
            "{}",
            text
        ),
    }
}

fn thread_name(processor_id: &str) -> String {
    // Thread names are limited to 15 chars on Linux; truncate the
    // processor id the same way the Python stderr-forwarder thread does.
    let short = &processor_id[..8.min(processor_id.len())];
    format!("br-{}", short)
}

fn write_frame<W: Write>(writer: &mut W, msg: &serde_json::Value) -> Result<()> {
    let bytes = serde_json::to_vec(msg)
        .map_err(|e| Error::Runtime(format!("failed to serialize bridge message: {e}")))?;
    let len = bytes.len() as u32;
    writer
        .write_all(&len.to_be_bytes())
        .map_err(|e| Error::Runtime(format!("failed to write bridge frame: {e}")))?;
    writer
        .write_all(&bytes)
        .map_err(|e| Error::Runtime(format!("failed to write bridge frame: {e}")))?;
    writer
        .flush()
        .map_err(|e| Error::Runtime(format!("failed to flush bridge frame: {e}")))?;
    Ok(())
}

fn read_frame<R: Read>(reader: &mut R) -> Result<serde_json::Value> {
    let mut len_buf = [0u8; 4];
    reader
        .read_exact(&mut len_buf)
        .map_err(|e| Error::Runtime(format!("bridge read failed: {e}")))?;
    let len = u32::from_be_bytes(len_buf) as usize;
    let mut buf = vec![0u8; len];
    reader
        .read_exact(&mut buf)
        .map_err(|e| Error::Runtime(format!("bridge read failed: {e}")))?;
    serde_json::from_slice(&buf)
        .map_err(|e| Error::Runtime(format!("bridge frame decode failed: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::core::context::{GpuContext, GpuContextLimitedAccess};
    use std::sync::mpsc::RecvTimeoutError;

    /// The routing decision, taken on the rpc tag alone and testable without a
    /// device: `SubprocessBridge` needs a GPU capability to construct, so the
    /// classification these cover would otherwise only run on the rig.
    mod incoming_frame_routing {
        use super::*;

        #[test]
        fn a_lifecycle_reply_is_routed_to_the_lifecycle_queue() {
            assert_eq!(
                classify_an_incoming_subprocess_frame(&serde_json::json!({"rpc": "ready"})),
                IncomingSubprocessFrame::LifecycleReply
            );
        }

        #[test]
        fn an_escalate_request_is_routed_to_the_escalate_dispatch() {
            assert_eq!(
                classify_an_incoming_subprocess_frame(&log_frame()),
                IncomingSubprocessFrame::EscalateRequest
            );
        }

        #[test]
        fn a_wire_answer_is_routed_to_its_link_rather_than_to_the_lifecycle_queue() {
            assert_eq!(
                classify_an_incoming_subprocess_frame(&serde_json::json!({
                    "rpc": OUT_OF_PROCESS_LINK_WIRED_RPC,
                    "link_id": "L-late",
                })),
                IncomingSubprocessFrame::LinkWireAnswer {
                    link_id: "L-late".to_string(),
                    outcome: OutOfProcessLinkWireOutcome::OpenedByTheFarSide,
                },
                "routed to the lifecycle queue it would be read as the answer to the next \
                 command the host sends"
            );
        }

        #[test]
        fn a_refused_wire_carries_the_subprocesss_own_reason() {
            assert_eq!(
                classify_an_incoming_subprocess_frame(&serde_json::json!({
                    "rpc": OUT_OF_PROCESS_LINK_WIRE_FAILED_RPC,
                    "link_id": "L-late",
                    "reason": "ExceedsMaxSupportedSubscribers",
                })),
                IncomingSubprocessFrame::LinkWireAnswer {
                    link_id: "L-late".to_string(),
                    outcome: OutOfProcessLinkWireOutcome::RefusedByTheFarSide {
                        reason: "ExceedsMaxSupportedSubscribers".to_string(),
                    },
                }
            );
        }

        #[test]
        fn a_refusal_that_says_nothing_still_carries_a_reason() {
            let IncomingSubprocessFrame::LinkWireAnswer {
                outcome: OutOfProcessLinkWireOutcome::RefusedByTheFarSide { reason },
                ..
            } = classify_an_incoming_subprocess_frame(&serde_json::json!({
                "rpc": OUT_OF_PROCESS_LINK_WIRE_FAILED_RPC,
                "link_id": "L-late",
            }))
            else {
                panic!("a refusal must classify as one whether or not it said why");
            };
            assert!(
                !reason.is_empty(),
                "`graph` renders this reason, so an empty one tells a reader nothing"
            );
        }

        #[test]
        fn a_wire_answer_naming_no_link_is_never_read_as_a_lifecycle_reply() {
            assert_eq!(
                classify_an_incoming_subprocess_frame(&serde_json::json!({
                    "rpc": OUT_OF_PROCESS_LINK_WIRED_RPC,
                })),
                IncomingSubprocessFrame::LinkWireAnswerNamingNoLink
            );
        }
    }

    fn gpu_or_skip(test_name: &str) -> Option<GpuContext> {
        match GpuContext::init_for_platform_sync() {
            Ok(gpu) => Some(gpu),
            Err(e) => {
                println!("{test_name}: no GPU device ({e}) — skipping");
                None
            }
        }
    }

    fn gpu_sandbox_or_skip(test_name: &str) -> Option<GpuContextLimitedAccess> {
        gpu_or_skip(test_name).map(GpuContextLimitedAccess::new)
    }

    fn log_frame() -> serde_json::Value {
        serde_json::json!({
            "rpc": "escalate_request",
            "op": "log",
            "source": "python",
            "source_seq": "1",
            "source_ts": "1970-01-01T00:00:00Z",
            "level": "info",
            "message": "hello from subprocess",
            "intercepted": false,
            "channel": serde_json::Value::Null,
            "pipeline_id": serde_json::Value::Null,
            "processor_id": "p-bridge-test",
            "attrs": {},
        })
    }

    // Regression gate for the fire-and-forget classification in `reader_loop`
    // (`if is_escalate_request { … } else { lifecycle_tx.send(msg) }`).
    //
    // Before the fix, `process_bridge_message` returning `None` for log ops
    // was indistinguishable from "this frame isn't an escalate request", so
    // the reader forwarded every log frame to the lifecycle channel. The
    // first host-side `bridge_recv()` after `setup` then saw the log frame
    // in place of `{"rpc":"ready"}` and reported `setup failed: unknown`.
    //
    // This test drives a real reader_loop over a `UnixStream::pair()` and
    // asserts that a log frame arriving on the bridge does not leak to
    // `lifecycle_rx`. Reverting the reader_loop classification change will
    // turn this test red.
    #[test]
    fn log_frame_does_not_leak_to_lifecycle_channel() {
        const TEST: &str = "log_frame_does_not_leak_to_lifecycle_channel";
        let Some(sandbox) = gpu_sandbox_or_skip(TEST) else {
            return;
        };

        let (parent_end, child_end) = UnixStream::pair().expect("socketpair");
        let bridge = SubprocessBridge::new(parent_end, sandbox, "p-bridge-test".into())
            .expect("bridge construction");

        // Keep the child stream alive across the entire test so the reader
        // loop stays in its read → classify → continue cycle instead of
        // hitting EOF and dropping `lifecycle_tx` (which would mask a real
        // leak as `Disconnected`).
        let mut child_writer = BufWriter::new(child_end);
        write_frame(&mut child_writer, &log_frame()).expect("write log frame");
        child_writer.flush().expect("flush");

        // If reader_loop regresses, the log frame arrives on lifecycle_rx
        // within a few ms. With the fix, it's consumed by
        // `process_bridge_message` and the lifecycle channel stays empty.
        let result = bridge.recv_lifecycle_timeout(Duration::from_millis(250));
        match result {
            Err(RecvTimeoutError::Timeout) => {}
            Ok(frame) => panic!(
                "log frame leaked to lifecycle channel \
                 — reader_loop classification has regressed: {frame}"
            ),
            Err(RecvTimeoutError::Disconnected) => panic!(
                "bridge reader exited before the test could assert — \
                 check for panics in the reader thread"
            ),
        }

        // Hold the child side open until the assertion completes.
        drop(child_writer);
    }

    // Positive control: a genuine lifecycle frame MUST arrive on
    // `lifecycle_rx`. Pairs with the negative test above to prove the
    // classification is a shift, not a blanket block.
    #[test]
    fn lifecycle_frame_still_routes_to_lifecycle_channel() {
        const TEST: &str = "lifecycle_frame_still_routes_to_lifecycle_channel";
        let Some(sandbox) = gpu_sandbox_or_skip(TEST) else {
            return;
        };

        let (parent_end, child_end) = UnixStream::pair().expect("socketpair");
        let bridge = SubprocessBridge::new(parent_end, sandbox, "p-bridge-test".into())
            .expect("bridge construction");

        let ready = serde_json::json!({"rpc": "ready"});
        let mut child_writer = BufWriter::new(child_end);
        write_frame(&mut child_writer, &ready).expect("write ready frame");
        drop(child_writer);

        let got = bridge
            .recv_lifecycle_timeout(Duration::from_millis(500))
            .expect("lifecycle frame must route through");
        assert_eq!(got.get("rpc").and_then(|v| v.as_str()), Some("ready"));
    }

    #[test]
    fn the_compiled_engine_build_id_leads_with_this_crates_version() {
        assert!(
            ENGINE_BUILD_ID.starts_with(concat!(env!("CARGO_PKG_VERSION"), "+")),
            "{ENGINE_BUILD_ID}"
        );
    }

    /// The crash-path mirror of the explicit `release_handle` op (#1901): a
    /// helper that dies without releasing its escalate acquires must not
    /// leave their surface-share registrations behind. The disconnect
    /// watchdog rightly never reclaims them — the host's own connection
    /// registered them on the helper's behalf, and the watchdog skips
    /// same-process peers — so bridge teardown is the only reclaimer.
    ///
    /// Each cycle acquires both kinds the drain distinguishes — a pixel
    /// buffer and a texture, the latter carrying the produce/consume
    /// timeline-fd pair the ticket names — and runs two crash-respawn
    /// cycles because the leak's bite is accumulation across respawns.
    /// Asserts the post-teardown table equals the post-acquire table minus
    /// exactly the helper's handles, so teardown is also shown to leave
    /// the pool's own long-lived registrations alone. Mental-revert: drop
    /// the surface-share half from the `SubprocessBridge::drop` drain and
    /// the post-drop assertion goes red on the first cycle.
    /// GPU-gated: skips when no device is present.
    #[test]
    #[cfg(target_os = "linux")]
    fn bridge_drop_releases_a_crashed_helpers_surface_share_registrations() {
        const TEST: &str = "bridge_drop_releases_a_crashed_helpers_surface_share_registrations";
        use std::collections::HashSet;

        use crate::core::context::SurfaceStore;
        use crate::linux::surface_share::{SurfaceShareState, UnixSocketSurfaceService};

        let Some(gpu) = gpu_or_skip(TEST) else {
            return;
        };

        let state = SurfaceShareState::new();
        let socket_dir = tempfile::TempDir::new().expect("socket dir");
        let socket_path = socket_dir.path().join("bridge-teardown.sock");
        let mut service = UnixSocketSurfaceService::new(state.clone(), socket_path.clone());
        service.start().expect("service start");

        let runtime_id = "bridge-teardown-test-runtime";
        let store = SurfaceStore::new(
            socket_path.to_string_lossy().into_owned(),
            runtime_id.to_string(),
        );
        store
            .connect()
            .expect("connect to the test surface-share service");
        gpu.set_surface_store(store);
        let sandbox = GpuContextLimitedAccess::new(gpu);
        let registered_surface_ids = || -> HashSet<String> {
            state
                .surface_ids_by_runtime(runtime_id)
                .into_iter()
                .collect()
        };

        for cycle in 0..2 {
            let (parent_end, child_end) = UnixStream::pair().expect("socketpair");
            let bridge =
                SubprocessBridge::new(parent_end, sandbox.clone(), format!("p-crash-{cycle}"))
                    .expect("bridge construction");

            let mut child_writer = BufWriter::new(child_end.try_clone().expect("clone child end"));
            let mut child_reader = BufReader::new(child_end);
            let mut acquire_via_bridge = |request: serde_json::Value| -> String {
                let op = request["op"]
                    .as_str()
                    .expect("request names an op")
                    .to_string();
                write_frame(&mut child_writer, &request).expect("write acquire frame");
                let response = read_frame(&mut child_reader).expect("acquire response");
                assert_eq!(
                    response.get("result").and_then(|v| v.as_str()),
                    Some("ok"),
                    "cycle {cycle}: {op} failed: {response}"
                );
                response
                    .get("handle_id")
                    .and_then(|v| v.as_str())
                    .expect("ok response carries a handle_id")
                    .to_string()
            };
            let pixel_buffer_handle_id = acquire_via_bridge(serde_json::json!({
                "rpc": "escalate_request",
                "op": "acquire_pixel_buffer",
                "request_id": format!("r-crash-buffer-{cycle}"),
                "width": 64,
                "height": 64,
                "format": "bgra",
            }));
            let texture_handle_id = acquire_via_bridge(serde_json::json!({
                "rpc": "escalate_request",
                "op": "acquire_texture",
                "request_id": format!("r-crash-texture-{cycle}"),
                "width": 64,
                "height": 64,
                "format": "rgba8_unorm",
                "usage": ["texture_binding", "copy_src"],
            }));

            // The acquires register their check-in ids, and (first cycle
            // only) create the pixel-buffer pool, whose pre-allocated slots
            // register themselves for cross-process lookup. Those pool
            // registrations live as long as the runtime — teardown must
            // release the helper's acquires and leave them alone.
            let registered_after_acquire = registered_surface_ids();
            for helper_handle_id in [&pixel_buffer_handle_id, &texture_handle_id] {
                assert!(
                    registered_after_acquire.contains(helper_handle_id),
                    "cycle {cycle}: the acquire must have registered '{helper_handle_id}' \
                     with the surface-share service"
                );
            }

            // The crash: the helper dies without a release_handle; bridge
            // teardown is everything that runs.
            drop(child_writer);
            drop(child_reader);
            drop(bridge);

            let mut expected_after_teardown = registered_after_acquire.clone();
            expected_after_teardown.remove(&pixel_buffer_handle_id);
            expected_after_teardown.remove(&texture_handle_id);
            let registered_after_teardown = registered_surface_ids();
            assert_eq!(
                registered_after_teardown, expected_after_teardown,
                "cycle {cycle}: bridge teardown must release the crashed helper's \
                 surface-share registrations and nothing else"
            );
        }

        service.stop();
    }
}
