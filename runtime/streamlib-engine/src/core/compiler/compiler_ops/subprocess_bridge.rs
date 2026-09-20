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
//! 3. Link wiring after setup (`wire_link`, `unwire_link`) — sent by
//!    [`SubprocessBridgeLinkDelivery`]; the subprocess answers a `wire_link`
//!    with [`OUT_OF_PROCESS_LINK_WIRED_RPC`] or
//!    [`OUT_OF_PROCESS_LINK_WIRE_FAILED_RPC`], naming the link it is about, and
//!    leaves an `unwire_link` unanswered.
//!
//! A dedicated reader thread (`br-…`) owns the parent-side read half and only
//! demultiplexes: a log record is handed to the log pipeline, a link's wire
//! answer lands on that link's own cell, a lifecycle reply is forwarded to the
//! main thread over an mpsc channel, and an escalate request that waits on an
//! answer is queued for the helper's one escalate worker (`br-esc-…`), which
//! dispatches through [`subprocess_escalate::process_bridge_message`] in
//! arrival order. Nothing the reader does waits on GPU work, so one slow
//! escalate never delays the helper's log records or its lifecycle replies,
//! and the helper's writes never back up behind it. The third role has its own
//! tags for the same reason the lifecycle queue is kept clean: an answer routed
//! there would be read as the reply to whatever command the host sends next.
//! Every frame the parent writes goes through one writer lock, so no two
//! threads interleave halves of a length-prefixed frame.

use std::io::{BufReader, BufWriter, Read, Write};
use std::os::unix::io::{AsRawFd, RawFd};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crate::core::context::GpuContextLimitedAccess;
use crate::core::error::{Error, PortDirection, Result};
use crate::core::processors::{
    LinksAwaitingTheirOutOfProcessWireReply, OutOfProcessFarSideLinkDelivery,
    OutOfProcessLinkWireOutcome, OutOfProcessLinkWireReply,
};
use crate::core::runtime::mesh::MeshLinkIngressTable;

use super::subprocess_escalate::{
    ESCALATE_OP_ANSWERED_BY_NOTHING, EscalateHandleRegistry, process_bridge_message,
    refusal_of_an_escalate_request, release_surface_share_and_texture_cache_for_handle,
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

/// How many escalate requests one helper may have queued for its worker before
/// the next is refused.
///
/// The shipped helper blocks each calling thread on its answer, so the queue
/// holds one request per thread; the bound only stops a helper that writes
/// requests without waiting from growing the app process's memory.
const ESCALATE_REQUESTS_QUEUED_PER_HELPER: usize = 256;

/// What handles an escalate request's frame and returns the frame to answer it
/// with, or `None` for an op answered by nothing.
///
/// The escalate dispatch in production; a stand-in under test, so the reader
/// and the worker are exercised without a GPU device.
type EscalateRequestDispatch =
    Arc<dyn Fn(&serde_json::Value) -> Option<serde_json::Value> + Send + Sync>;

/// Where one frame arriving from the subprocess belongs.
///
/// Classified on the frame's tags alone, never on what a handler makes of the
/// frame: the routing decision is the wire contract's, and a frame that fell
/// through to the lifecycle queue by accident is read as the answer to the next
/// command the host sends.
#[derive(Debug, PartialEq, Eq)]
enum IncomingSubprocessFrame {
    /// An escalate request the helper waits on no answer for — a log record —
    /// dispatched on the reader thread the moment it arrives.
    EscalateRequestAnsweredByNothing,
    /// An escalate request the helper waits on an answer for, queued for the
    /// helper's escalate worker.
    EscalateRequestAwaitingAnAnswer,
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
        return match msg.get("op").and_then(|op| op.as_str()) {
            Some(ESCALATE_OP_ANSWERED_BY_NOTHING) => {
                IncomingSubprocessFrame::EscalateRequestAnsweredByNothing
            }
            _ => IncomingSubprocessFrame::EscalateRequestAwaitingAnAnswer,
        };
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

/// What a link wired into a helper process that has failed is refused with.
pub fn refusal_of_a_link_into_a_helper_process_that_failed(
    processor_display_name: &str,
    processor_id: &str,
) -> String {
    format!(
        "processor '{processor_display_name}' ({processor_id}) has failed, so no link can be \
         wired into it"
    )
}

/// The parent's side of one subprocess's socket as every bridge thread shares
/// it: the framed writer, whether the parent has given up on the subprocess,
/// and the links the subprocess still owes an answer for — which the reader
/// routes answers to and giving up refuses.
///
/// The writer's lock keeps two threads from interleaving halves of a
/// length-prefixed frame.
struct ParentSideOfOneSubprocessBridgeSharedByItsThreads {
    processor_id: String,
    framed_socket_writer: parking_lot::Mutex<BufWriter<UnixStream>>,
    /// Shut down outside the writer's lock, so teardown never waits behind a
    /// write the subprocess is not reading.
    socket_for_shutting_down: UnixStream,
    the_subprocess_was_given_up_on: AtomicBool,
    links_awaiting_their_wire_reply: LinksAwaitingTheirOutOfProcessWireReply,
}

impl ParentSideOfOneSubprocessBridgeSharedByItsThreads {
    fn over(parent_end: UnixStream, processor_id: String) -> Result<Self> {
        let socket_for_shutting_down = parent_end.try_clone().map_err(|e| {
            Error::Runtime(format!(
                "failed to clone escalate socketpair for shutdown: {e}"
            ))
        })?;
        Ok(Self {
            processor_id,
            framed_socket_writer: parking_lot::Mutex::new(BufWriter::new(parent_end)),
            socket_for_shutting_down,
            the_subprocess_was_given_up_on: AtomicBool::new(false),
            links_awaiting_their_wire_reply: LinksAwaitingTheirOutOfProcessWireReply::default(),
        })
    }

    fn the_subprocess_was_given_up_on(&self) -> bool {
        self.the_subprocess_was_given_up_on.load(Ordering::SeqCst)
    }

    /// Give the subprocess up and refuse every link it still owed an answer
    /// for: a link whose far side is gone with the answer outstanding never
    /// reads `wired`.
    fn give_up_on_the_subprocess(&self, how_the_subprocess_was_noticed_gone: &str) {
        self.the_subprocess_was_given_up_on
            .store(true, Ordering::SeqCst);
        refuse_every_link_this_subprocess_still_owed(
            &self.links_awaiting_their_wire_reply,
            &self.processor_id,
            how_the_subprocess_was_noticed_gone,
        );
    }

    /// Write one frame whole, giving the subprocess up when it cannot be
    /// written.
    fn write_frame_or_give_up_on_the_subprocess(&self, frame: &serde_json::Value) -> Result<()> {
        let written = write_frame(&mut *self.framed_socket_writer.lock(), frame);
        if written.is_err() {
            self.give_up_on_the_subprocess("is gone");
        }
        written
    }

    fn shut_the_socket_down(&self) {
        let _ = self
            .socket_for_shutting_down
            .shutdown(std::net::Shutdown::Both);
    }
}

/// Bridge for one subprocess. Drop the value to tear its threads down: the
/// socket is shut down, the reader sees EOF, and the escalate worker leaves
/// once the reader has.
pub struct SubprocessBridge {
    parent_side: Arc<ParentSideOfOneSubprocessBridgeSharedByItsThreads>,
    lifecycle_rx: Receiver<serde_json::Value>,
    registry: Arc<EscalateHandleRegistry>,
    /// Held for teardown: the drop path evicts what the registry's acquires
    /// entered into the parent's texture cache, which needs the same
    /// capability the escalate worker dispatches against.
    sandbox: GpuContextLimitedAccess,
    frame_demultiplexing_reader_thread: Option<JoinHandle<()>>,
    escalate_worker_thread: Option<JoinHandle<()>>,
}

impl SubprocessBridge {
    /// Wrap a socketpair parent end and spawn the reader thread and the
    /// escalate worker, which dispatches against `sandbox` and, for the one
    /// question a helper cannot answer for itself, against this runtime's mesh.
    pub fn new(
        stream: UnixStream,
        sandbox: GpuContextLimitedAccess,
        mesh_link_ingress_table: Arc<MeshLinkIngressTable>,
        processor_id: String,
    ) -> Result<Self> {
        let read_half = stream.try_clone().map_err(|e| {
            Error::Runtime(format!(
                "failed to clone escalate socketpair for reader: {e}"
            ))
        })?;
        let parent_side = Arc::new(ParentSideOfOneSubprocessBridgeSharedByItsThreads::over(
            stream,
            processor_id,
        )?);
        let registry = EscalateHandleRegistry::new();
        let (lifecycle_tx, lifecycle_rx) = mpsc::channel();

        let dispatch_registry = Arc::clone(&registry);
        let dispatch_sandbox = sandbox.clone();
        let escalate_request_dispatch: EscalateRequestDispatch = Arc::new(move |frame| {
            process_bridge_message(
                &dispatch_sandbox,
                &dispatch_registry,
                &mesh_link_ingress_table,
                frame,
            )
        });
        let SubprocessBridgeThreads {
            frame_demultiplexing_reader_thread,
            escalate_worker_thread,
        } = spawn_the_reader_and_the_escalate_worker(
            BufReader::new(read_half),
            Arc::clone(&parent_side),
            escalate_request_dispatch,
            lifecycle_tx,
        )?;

        Ok(Self {
            parent_side,
            lifecycle_rx,
            registry,
            sandbox,
            frame_demultiplexing_reader_thread: Some(frame_demultiplexing_reader_thread),
            escalate_worker_thread: Some(escalate_worker_thread),
        })
    }

    /// Write a length-prefixed JSON lifecycle command to the subprocess.
    ///
    /// Link wiring never comes through here — see
    /// [`SubprocessBridgeLinkDelivery`] — because every command sent here is
    /// noted as the last lifecycle command the subprocess was sent.
    pub fn send(&self, msg: &serde_json::Value) -> Result<()> {
        if self.parent_side.the_subprocess_was_given_up_on() {
            return Err(Error::Runtime(format!(
                "[{}] bridge marked dead, cannot send",
                self.parent_side.processor_id
            )));
        }
        // The engine's only reading of which hook the child is inside.
        // Setup-phase-only escalate ops (minting a processor-owned window)
        // refuse on it, dispatched from the escalate worker while the hook
        // that is allowed to ask is still running.
        if let Some(lifecycle_command) = msg.get("cmd").and_then(|c| c.as_str()) {
            self.registry
                .note_lifecycle_command_sent_to_the_helper_process(lifecycle_command);
        }
        self.parent_side
            .write_frame_or_give_up_on_the_subprocess(msg)
    }

    /// Block until the next lifecycle-tagged message arrives.
    pub fn recv_lifecycle(&self) -> Result<serde_json::Value> {
        self.lifecycle_rx.recv().map_err(|_| {
            self.mark_dead();
            Error::Runtime(format!(
                "[{}] subprocess escalate socket closed before reply",
                self.parent_side.processor_id
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

    /// Mark the bridge dead; subsequent sends return immediately, and every
    /// link this subprocess was still to answer for is refused.
    pub fn mark_dead(&self) {
        self.parent_side.give_up_on_the_subprocess("is gone");
    }

    pub fn is_dead(&self) -> bool {
        self.parent_side.the_subprocess_was_given_up_on()
    }

    /// How a link wired after this subprocess's setup command reaches it, for
    /// its wiring envelope to hand links over through.
    pub fn link_delivery_to_this_subprocess(
        &self,
        processor_display_name: &str,
    ) -> SubprocessBridgeLinkDelivery {
        SubprocessBridgeLinkDelivery {
            processor_display_name: processor_display_name.to_string(),
            parent_side: Arc::clone(&self.parent_side),
        }
    }

    /// Count of escalate-acquired handles the host still holds. Used by
    /// teardown logging and tests.
    pub(crate) fn registry(&self) -> &Arc<EscalateHandleRegistry> {
        &self.registry
    }
}

/// How links wired after a subprocess's setup command reach it: `wire_link`
/// and `unwire_link` frames written straight onto its bridge's socket.
///
/// Never through [`SubprocessBridge::send`]: a link can be handed over while
/// the subprocess is still inside `setup()`, and noting `wire_link` as its last
/// lifecycle command would refuse the window that hook may mint.
pub struct SubprocessBridgeLinkDelivery {
    processor_display_name: String,
    parent_side: Arc<ParentSideOfOneSubprocessBridgeSharedByItsThreads>,
}

impl OutOfProcessFarSideLinkDelivery for SubprocessBridgeLinkDelivery {
    /// The answer arrives on its own rpc tag, which the reader routes to
    /// `answer_cell`. The send never waits on that answer: the compiler hands
    /// links over holding the graph's write lock.
    fn hand_over_a_link_wired_after_setup(
        &self,
        port_direction: PortDirection,
        link_wiring: &serde_json::Value,
        answer_cell: Arc<OutOfProcessLinkWireReply>,
    ) -> Result<()> {
        if self.parent_side.the_subprocess_was_given_up_on() {
            return Err(Error::Runtime(
                refusal_of_a_link_into_a_helper_process_that_failed(
                    &self.processor_display_name,
                    &self.parent_side.processor_id,
                ),
            ));
        }
        let Some(link_id) = link_wiring.get("link_id").and_then(|id| id.as_str()) else {
            return Err(Error::Configuration(format!(
                "the wiring handed to processor '{}' ({}) names no link, so its helper \
                 process could not answer for one",
                self.processor_display_name, self.parent_side.processor_id
            )));
        };
        // Registered before the send: the subprocess can answer the moment the
        // frame lands.
        self.parent_side
            .links_awaiting_their_wire_reply
            .await_an_answer_for_link(link_id.to_string(), answer_cell);
        self.parent_side
            .write_frame_or_give_up_on_the_subprocess(&serde_json::json!({
                "cmd": "wire_link",
                "direction": port_direction.as_wire_str(),
                "link": link_wiring,
            }))
    }

    /// Unanswered, like `run`: a reply nobody reads is read as the answer to
    /// the next lifecycle command.
    fn tell_the_far_side_a_link_was_unwired(
        &self,
        port_direction: PortDirection,
        local_port_name: &str,
        link_id: &str,
    ) -> Result<()> {
        // A link on its way out is owed no answer; left waiting, a subprocess
        // that dies later would refuse a link the graph no longer has.
        self.parent_side
            .links_awaiting_their_wire_reply
            .stop_awaiting_an_answer_for_link(link_id);
        if self.parent_side.the_subprocess_was_given_up_on() {
            return Ok(());
        }
        self.parent_side
            .write_frame_or_give_up_on_the_subprocess(&serde_json::json!({
                "cmd": "unwire_link",
                "direction": port_direction.as_wire_str(),
                "port": local_port_name,
                "link_id": link_id,
            }))
    }

    fn refuse_every_link_still_awaiting_the_far_sides_answer(&self, reason: &str) {
        self.parent_side
            .links_awaiting_their_wire_reply
            .refuse_every_link_still_awaiting_an_answer(reason);
    }
}

impl Drop for SubprocessBridge {
    fn drop(&mut self) {
        self.mark_dead();
        // Given up on and shut down before draining: the escalate worker passes
        // over every request still queued, and the reader queues nothing past
        // EOF, because an acquire landing after the drain would strand its
        // cache entry. A request the worker is already executing can still slip
        // through; closing that too would mean joining the worker, which this
        // path deliberately never blocks on.
        self.parent_side.shut_the_socket_down();
        // Windows first: each present thread resolves surface ids against the
        // same capability the handle release below evicts from, and dropping
        // one closes its window and joins its thread. A helper that never
        // called `close_processor_owned_window` — or crashed — releases its
        // windows here, which is what makes teardown the backstop the plan
        // says it is.
        for (window_id, present_loop) in self.registry.drain_processor_owned_windows() {
            tracing::debug!(
                "[{}] closing processor-owned window '{}' at teardown",
                self.parent_side.processor_id,
                window_id
            );
            // Closed explicitly rather than by dropping the `Arc`: a request
            // still in flight on the escalate worker can hold the last
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
        // Detached, never joined: the OS reaps both on process exit.
        self.frame_demultiplexing_reader_thread.take();
        self.escalate_worker_thread.take();
    }
}

/// The two threads one bridge runs.
struct SubprocessBridgeThreads {
    frame_demultiplexing_reader_thread: JoinHandle<()>,
    escalate_worker_thread: JoinHandle<()>,
}

/// Spawn the escalate worker, then the reader that feeds it.
///
/// The reader owns the only sender of the worker's queue, so the worker leaves
/// once the reader has and every queued request is taken off.
fn spawn_the_reader_and_the_escalate_worker(
    reader: BufReader<UnixStream>,
    parent_side: Arc<ParentSideOfOneSubprocessBridgeSharedByItsThreads>,
    escalate_request_dispatch: EscalateRequestDispatch,
    lifecycle_tx: mpsc::Sender<serde_json::Value>,
) -> Result<SubprocessBridgeThreads> {
    let (escalate_requests_tx, escalate_requests_rx) =
        mpsc::sync_channel(ESCALATE_REQUESTS_QUEUED_PER_HELPER);
    let processor_id = parent_side.processor_id.clone();

    let worker_parent_side = Arc::clone(&parent_side);
    let worker_dispatch = Arc::clone(&escalate_request_dispatch);
    let escalate_worker_thread = thread::Builder::new()
        .name(bridge_escalate_worker_thread_name(&processor_id))
        .spawn(move || {
            escalate_worker_loop(escalate_requests_rx, &worker_parent_side, &worker_dispatch);
        })
        .map_err(|spawn_failure| {
            Error::Runtime(format!(
                "[{processor_id}] could not start its bridge's escalate worker: {spawn_failure}"
            ))
        })?;

    let frame_demultiplexing_reader_thread = thread::Builder::new()
        .name(bridge_reader_thread_name(&processor_id))
        .spawn(move || {
            reader_loop(
                reader,
                &parent_side,
                &escalate_request_dispatch,
                escalate_requests_tx,
                lifecycle_tx,
            );
        })
        .map_err(|spawn_failure| {
            Error::Runtime(format!(
                "[{processor_id}] could not start its bridge's reader: {spawn_failure}"
            ))
        })?;

    Ok(SubprocessBridgeThreads {
        frame_demultiplexing_reader_thread,
        escalate_worker_thread,
    })
}

/// Reader loop: drain the parent-side socket and route every frame, never
/// waiting on the work a frame asks for.
fn reader_loop(
    mut reader: BufReader<UnixStream>,
    parent_side: &ParentSideOfOneSubprocessBridgeSharedByItsThreads,
    escalate_request_dispatch: &EscalateRequestDispatch,
    escalate_requests_tx: mpsc::SyncSender<serde_json::Value>,
    lifecycle_tx: mpsc::Sender<serde_json::Value>,
) {
    let processor_id = &parent_side.processor_id;
    loop {
        let msg = match read_frame(&mut reader) {
            Ok(v) => v,
            Err(e) => {
                tracing::debug!("[{}] bridge reader exiting: {}", processor_id, e);
                break;
            }
        };

        // Classify the frame on its tags, not the handler's reply shape: a log
        // record produces no response, so a `None` from the dispatch cannot be
        // the "this wasn't an escalate request" signal — that would silently
        // re-route every log record to the lifecycle queue and trip the
        // setup/teardown waiters.
        // Matched rather than compared arm by arm: a route added later must not
        // fall through to the lifecycle queue, which is the exact failure this
        // classification exists to prevent.
        match classify_an_incoming_subprocess_frame(&msg) {
            IncomingSubprocessFrame::LinkWireAnswer { link_id, outcome } => {
                if !parent_side
                    .links_awaiting_their_wire_reply
                    .note_the_far_sides_answer_for_link(&link_id, outcome)
                {
                    tracing::warn!(
                        "[{}] its helper process answered for link '{}', which no link is \
                         waiting on",
                        processor_id,
                        link_id
                    );
                }
                continue;
            }
            IncomingSubprocessFrame::LinkWireAnswerNamingNoLink => {
                tracing::warn!(
                    "[{}] its helper process answered a wire with no link named, so the link it \
                     meant stays unanswered",
                    processor_id
                );
                continue;
            }
            IncomingSubprocessFrame::LifecycleReply => {
                // Forwarded to the main thread below, where `msg` is still in
                // hand — the one route that needs the frame itself.
            }
            IncomingSubprocessFrame::EscalateRequestAnsweredByNothing => {
                // A log record lands in the log pipeline without touching the
                // device. Only a record that failed to decode is answered, with
                // the refusal.
                if let Some(refusal) = escalate_request_dispatch(&msg)
                    && parent_side
                        .write_frame_or_give_up_on_the_subprocess(&refusal)
                        .is_err()
                {
                    break;
                }
                continue;
            }
            IncomingSubprocessFrame::EscalateRequestAwaitingAnAnswer => {
                match escalate_requests_tx.try_send(msg) {
                    Ok(()) => {}
                    // Refused rather than waited for: a reader blocked on a full
                    // queue would hold this helper's log records and lifecycle
                    // replies behind its GPU work again.
                    Err(mpsc::TrySendError::Full(refused_request)) => {
                        let refusal = refusal_of_an_escalate_request(
                            &refused_request,
                            format!(
                                "the app process already holds \
                                 {ESCALATE_REQUESTS_QUEUED_PER_HELPER} escalate requests from \
                                 this helper process that it has not answered, so this one was \
                                 refused rather than queued"
                            ),
                        );
                        if parent_side
                            .write_frame_or_give_up_on_the_subprocess(&refusal)
                            .is_err()
                        {
                            break;
                        }
                    }
                    // The worker leaves before this sender is dropped only by
                    // panicking, and a helper whose requests nothing can answer
                    // is given up on.
                    Err(mpsc::TrySendError::Disconnected(_)) => {
                        tracing::warn!(
                            "[{}] its bridge's escalate worker is gone, so no escalate request \
                             it sends can be answered; giving up on the helper process",
                            processor_id
                        );
                        break;
                    }
                }
                continue;
            }
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
    // subprocess, so it is said once here rather than on each `break`.
    parent_side.give_up_on_the_subprocess("stopped answering");
}

/// Escalate worker loop: dispatch each queued request in the order the helper
/// sent it and write its answer, one at a time — a release never overtakes the
/// acquire it releases, and the helper holds the escalate gate for at most one
/// op.
///
/// A request still queued once the subprocess was given up on is passed over:
/// teardown's drain has released or will release what the helper held, and an
/// acquire dispatched after it would strand what it acquired.
fn escalate_worker_loop(
    escalate_requests_rx: Receiver<serde_json::Value>,
    parent_side: &ParentSideOfOneSubprocessBridgeSharedByItsThreads,
    escalate_request_dispatch: &EscalateRequestDispatch,
) {
    for request in escalate_requests_rx {
        if parent_side.the_subprocess_was_given_up_on() {
            continue;
        }
        let Some(response) = escalate_request_dispatch(&request) else {
            continue;
        };
        if let Err(write_failure) = parent_side.write_frame_or_give_up_on_the_subprocess(&response)
        {
            tracing::warn!(
                "[{}] bridge failed to write escalate response: {}",
                parent_side.processor_id,
                write_failure
            );
        }
    }
}

/// Refuse every link this subprocess was still to answer for, and say so once.
fn refuse_every_link_this_subprocess_still_owed(
    links_awaiting_their_wire_reply: &LinksAwaitingTheirOutOfProcessWireReply,
    processor_id: &str,
    how_the_helper_was_noticed_gone: &str,
) {
    let refused =
        links_awaiting_their_wire_reply.refuse_every_link_still_awaiting_an_answer(&format!(
            "the helper process hosting '{processor_id}' {how_the_helper_was_noticed_gone} before \
             it opened its port for this link"
        ));
    if refused > 0 {
        tracing::warn!(
            "[{}] its helper process {} with {} link(s) it never opened a port for; each reads \
             error rather than wired",
            processor_id,
            how_the_helper_was_noticed_gone,
            refused
        );
    }
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
    let name = format!(
        "{}-{}",
        thread_prefix,
        processor_id_shortened_for_a_thread_name(processor_id)
    );
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

/// At most the first eight characters of a processor id, cut on a character
/// boundary: Linux thread names are limited to 15 bytes.
fn processor_id_shortened_for_a_thread_name(processor_id: &str) -> &str {
    let end = processor_id
        .char_indices()
        .nth(8)
        .map_or(processor_id.len(), |(byte_index, _)| byte_index);
    &processor_id[..end]
}

fn bridge_reader_thread_name(processor_id: &str) -> String {
    format!(
        "br-{}",
        processor_id_shortened_for_a_thread_name(processor_id)
    )
}

fn bridge_escalate_worker_thread_name(processor_id: &str) -> String {
    format!(
        "br-esc-{}",
        processor_id_shortened_for_a_thread_name(processor_id)
    )
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

    use crate::core::runtime::mesh::a_mesh_link_ingress_table_carrying_nothing;

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
        fn a_log_record_is_dispatched_on_the_reader_the_moment_it_arrives() {
            assert_eq!(
                classify_an_incoming_subprocess_frame(&log_frame()),
                IncomingSubprocessFrame::EscalateRequestAnsweredByNothing
            );
        }

        #[test]
        fn an_escalate_request_awaiting_an_answer_is_queued_for_the_escalate_worker() {
            assert_eq!(
                classify_an_incoming_subprocess_frame(&escalate_request_awaiting_an_answer(
                    "run_compute_kernel",
                    "r-queued"
                )),
                IncomingSubprocessFrame::EscalateRequestAwaitingAnAnswer
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

    /// The reader and the escalate worker over a real socketpair, with a
    /// stand-in dispatch in place of the GPU one, so what the reader never waits
    /// on is provable without a device.
    mod reader_and_escalate_worker {
        use super::*;
        use std::sync::Mutex;
        use std::sync::atomic::{AtomicUsize, Ordering};

        const LONGEST_THE_READER_MAY_TAKE_TO_ROUTE_A_FRAME: Duration = Duration::from_secs(5);

        /// One bridge's two threads, with the helper's end of the socket in the
        /// test's hands.
        struct BridgeThreadsDrivenFromTheHelperEnd {
            helper_end_writer: BufWriter<UnixStream>,
            helper_end_reader: BufReader<UnixStream>,
            lifecycle_rx: Receiver<serde_json::Value>,
            parent_side: Arc<ParentSideOfOneSubprocessBridgeSharedByItsThreads>,
            threads: SubprocessBridgeThreads,
        }

        impl BridgeThreadsDrivenFromTheHelperEnd {
            fn dispatching_through(escalate_request_dispatch: EscalateRequestDispatch) -> Self {
                let (parent_end, helper_end) = UnixStream::pair().expect("socketpair");
                let parent_read_half = parent_end.try_clone().expect("clone the parent end");
                let (lifecycle_tx, lifecycle_rx) = mpsc::channel();
                let parent_side = Arc::new(
                    ParentSideOfOneSubprocessBridgeSharedByItsThreads::over(
                        parent_end,
                        "p-escalate-worker-test".to_string(),
                    )
                    .expect("clone the parent end"),
                );
                let threads = spawn_the_reader_and_the_escalate_worker(
                    BufReader::new(parent_read_half),
                    Arc::clone(&parent_side),
                    escalate_request_dispatch,
                    lifecycle_tx,
                )
                .expect("spawn the bridge's threads");
                Self {
                    helper_end_writer: BufWriter::new(
                        helper_end.try_clone().expect("clone the helper end"),
                    ),
                    helper_end_reader: BufReader::new(helper_end),
                    lifecycle_rx,
                    parent_side,
                    threads,
                }
            }

            fn send_from_the_helper(&mut self, frame: &serde_json::Value) {
                write_frame(&mut self.helper_end_writer, frame).expect("write from the helper end");
            }

            fn request_id_of_the_next_answer_the_helper_reads(&mut self) -> String {
                read_frame(&mut self.helper_end_reader)
                    .expect("read an answer at the helper end")
                    .get("request_id")
                    .and_then(|request_id| request_id.as_str())
                    .expect("the answer names its request")
                    .to_string()
            }
        }

        /// A stand-in dispatch that holds any request for `HELD_OP` until the
        /// test lets it go, reports every log record and every request it
        /// starts, and answers each request with its own id.
        struct EscalateDispatchHoldingOneOp {
            escalate_request_dispatch: EscalateRequestDispatch,
            let_the_held_request_go: mpsc::Sender<()>,
            request_ids_started: Receiver<String>,
            log_records_dispatched: Receiver<String>,
        }

        const HELD_OP: &str = "register_compute_kernel";

        impl EscalateDispatchHoldingOneOp {
            fn new() -> Self {
                let (let_the_held_request_go, held_request_gate) = mpsc::channel::<()>();
                let held_request_gate = Mutex::new(held_request_gate);
                let (request_started_tx, request_ids_started) = mpsc::channel();
                let request_started_tx = Mutex::new(request_started_tx);
                let (log_record_tx, log_records_dispatched) = mpsc::channel();
                let log_record_tx = Mutex::new(log_record_tx);
                let escalate_request_dispatch: EscalateRequestDispatch = Arc::new(move |frame| {
                    let op = frame.get("op").and_then(|op| op.as_str()).unwrap_or("");
                    if op == ESCALATE_OP_ANSWERED_BY_NOTHING {
                        let message = frame["message"].as_str().unwrap_or("").to_string();
                        let _ = log_record_tx.lock().unwrap().send(message);
                        return None;
                    }
                    let request_id = frame["request_id"].as_str().unwrap_or("").to_string();
                    let _ = request_started_tx.lock().unwrap().send(request_id.clone());
                    if op == HELD_OP {
                        let _ = held_request_gate.lock().unwrap().recv();
                    }
                    Some(serde_json::json!({
                        "rpc": "escalate_response",
                        "request_id": request_id,
                        "result": "ok",
                    }))
                });
                Self {
                    escalate_request_dispatch,
                    let_the_held_request_go,
                    request_ids_started,
                    log_records_dispatched,
                }
            }
        }

        /// Fail-without-fix: dispatch escalates inline on the reader and neither
        /// the log record nor `ready` is routed until the held op returns, so
        /// both waits below run out.
        #[test]
        fn a_slow_escalate_never_delays_a_log_record_or_a_lifecycle_reply_sent_behind_it() {
            let dispatch = EscalateDispatchHoldingOneOp::new();
            let mut bridge = BridgeThreadsDrivenFromTheHelperEnd::dispatching_through(Arc::clone(
                &dispatch.escalate_request_dispatch,
            ));

            bridge.send_from_the_helper(&escalate_request_awaiting_an_answer(HELD_OP, "r-slow"));
            bridge.send_from_the_helper(&log_frame());
            bridge.send_from_the_helper(&serde_json::json!({"rpc": "ready"}));

            assert_eq!(
                dispatch
                    .request_ids_started
                    .recv_timeout(LONGEST_THE_READER_MAY_TAKE_TO_ROUTE_A_FRAME)
                    .expect("the slow request reaches the worker"),
                "r-slow"
            );
            assert_eq!(
                dispatch
                    .log_records_dispatched
                    .recv_timeout(LONGEST_THE_READER_MAY_TAKE_TO_ROUTE_A_FRAME)
                    .expect(
                        "a log record sent behind a held escalate is dispatched while it is held"
                    ),
                "hello from subprocess"
            );
            let lifecycle_reply = bridge
                .lifecycle_rx
                .recv_timeout(LONGEST_THE_READER_MAY_TAKE_TO_ROUTE_A_FRAME)
                .expect("a lifecycle reply sent behind a held escalate is routed while it is held");
            assert_eq!(lifecycle_reply["rpc"], "ready");

            dispatch
                .let_the_held_request_go
                .send(())
                .expect("the held request is still waiting");
            assert_eq!(
                bridge.request_id_of_the_next_answer_the_helper_reads(),
                "r-slow",
                "the held request is still answered once it returns"
            );
        }

        /// Fail-without-fix: dispatch each request on its own thread and the
        /// requests overlap and answer out of the order they arrived in.
        #[test]
        fn escalates_from_one_helper_are_answered_one_at_a_time_in_the_order_they_arrived() {
            let requests_in_flight = Arc::new(AtomicUsize::new(0));
            let most_requests_ever_in_flight = Arc::new(AtomicUsize::new(0));
            let escalate_request_dispatch: EscalateRequestDispatch = {
                let requests_in_flight = Arc::clone(&requests_in_flight);
                let most_requests_ever_in_flight = Arc::clone(&most_requests_ever_in_flight);
                Arc::new(move |frame| {
                    let in_flight = requests_in_flight.fetch_add(1, Ordering::SeqCst) + 1;
                    most_requests_ever_in_flight.fetch_max(in_flight, Ordering::SeqCst);
                    std::thread::sleep(Duration::from_millis(2));
                    requests_in_flight.fetch_sub(1, Ordering::SeqCst);
                    Some(serde_json::json!({
                        "rpc": "escalate_response",
                        "request_id": frame["request_id"],
                        "result": "ok",
                    }))
                })
            };
            let mut bridge =
                BridgeThreadsDrivenFromTheHelperEnd::dispatching_through(escalate_request_dispatch);

            let request_ids: Vec<String> = (0..8).map(|index| format!("r-{index}")).collect();
            for request_id in &request_ids {
                bridge.send_from_the_helper(&escalate_request_awaiting_an_answer(
                    "run_compute_kernel",
                    request_id,
                ));
            }

            let answered_in_order: Vec<String> = request_ids
                .iter()
                .map(|_| bridge.request_id_of_the_next_answer_the_helper_reads())
                .collect();
            assert_eq!(answered_in_order, request_ids);
            assert_eq!(
                most_requests_ever_in_flight.load(Ordering::SeqCst),
                1,
                "one helper's escalates never run concurrently"
            );
        }

        /// A request past the helper's queue bound is refused at once, so the
        /// reader keeps routing the helper's lifecycle replies behind it.
        ///
        /// Fail-without-fix: block the reader until the queue has room and
        /// neither the refusal nor `ready` arrives while the held op runs.
        #[test]
        fn a_request_past_the_helpers_queue_bound_is_refused_rather_than_stalling_the_reader() {
            let dispatch = EscalateDispatchHoldingOneOp::new();
            let mut bridge = BridgeThreadsDrivenFromTheHelperEnd::dispatching_through(Arc::clone(
                &dispatch.escalate_request_dispatch,
            ));
            bridge.send_from_the_helper(&escalate_request_awaiting_an_answer(HELD_OP, "r-held"));
            dispatch
                .request_ids_started
                .recv_timeout(LONGEST_THE_READER_MAY_TAKE_TO_ROUTE_A_FRAME)
                .expect("the held request reaches the worker");

            for queued_index in 0..ESCALATE_REQUESTS_QUEUED_PER_HELPER {
                bridge.send_from_the_helper(&escalate_request_awaiting_an_answer(
                    "run_compute_kernel",
                    &format!("r-queued-{queued_index}"),
                ));
            }
            bridge.send_from_the_helper(&escalate_request_awaiting_an_answer(
                "run_compute_kernel",
                "r-past-the-bound",
            ));
            bridge.send_from_the_helper(&serde_json::json!({"rpc": "ready"}));

            bridge
                .helper_end_reader
                .get_ref()
                .set_read_timeout(Some(LONGEST_THE_READER_MAY_TAKE_TO_ROUTE_A_FRAME))
                .expect("a read timeout on the helper end");
            let refusal = read_frame(&mut bridge.helper_end_reader)
                .expect("the request past the bound is answered while the held op runs");
            assert_eq!(refusal["request_id"], "r-past-the-bound");
            assert_eq!(refusal["result"], "err");
            let lifecycle_reply = bridge
                .lifecycle_rx
                .recv_timeout(LONGEST_THE_READER_MAY_TAKE_TO_ROUTE_A_FRAME)
                .expect("a lifecycle reply behind a full queue is still routed");
            assert_eq!(lifecycle_reply["rpc"], "ready");

            dispatch
                .let_the_held_request_go
                .send(())
                .expect("the held request is still waiting");
        }

        /// Fail-without-fix: drop the given-up check in the worker and the queued
        /// acquire is dispatched after the bridge gave up, past the teardown
        /// drain that would have released it.
        #[test]
        fn an_escalate_still_queued_when_its_bridge_gives_up_is_never_dispatched() {
            let dispatch = EscalateDispatchHoldingOneOp::new();
            let mut bridge = BridgeThreadsDrivenFromTheHelperEnd::dispatching_through(Arc::clone(
                &dispatch.escalate_request_dispatch,
            ));

            bridge.send_from_the_helper(&escalate_request_awaiting_an_answer(HELD_OP, "r-held"));
            bridge.send_from_the_helper(&escalate_request_awaiting_an_answer(
                "acquire_pixel_buffer",
                "r-queued-behind-it",
            ));
            assert_eq!(
                dispatch
                    .request_ids_started
                    .recv_timeout(LONGEST_THE_READER_MAY_TAKE_TO_ROUTE_A_FRAME)
                    .expect("the held request reaches the worker"),
                "r-held"
            );

            bridge.parent_side.give_up_on_the_subprocess("is gone");
            dispatch
                .let_the_held_request_go
                .send(())
                .expect("the held request is still waiting");
            let BridgeThreadsDrivenFromTheHelperEnd {
                helper_end_writer,
                helper_end_reader,
                threads,
                ..
            } = bridge;
            drop(helper_end_writer);
            drop(helper_end_reader);
            threads
                .frame_demultiplexing_reader_thread
                .join()
                .expect("the reader leaves at EOF");
            threads
                .escalate_worker_thread
                .join()
                .expect("the worker leaves once the reader has");

            let started_after_the_bridge_gave_up: Vec<String> =
                dispatch.request_ids_started.try_iter().collect();
            assert!(
                started_after_the_bridge_gave_up.is_empty(),
                "a request queued behind the give-up was dispatched: \
                 {started_after_the_bridge_gave_up:?}"
            );
        }
    }

    /// Links handed to a subprocess after its setup command, over a real
    /// socketpair, with no GPU capability in reach.
    mod link_delivery_to_a_subprocess_past_its_setup_command {
        use super::*;
        use crate::core::execution::ProcessExecution;
        use crate::core::processors::OutOfProcessLinkWiringEnvelope;
        use crate::core::test_support::CapturedTracingWarnings;

        struct LinkDeliveryWithTheSubprocessEndInHand {
            delivery: SubprocessBridgeLinkDelivery,
            subprocess_end_reader: BufReader<UnixStream>,
        }

        fn link_delivery_with_the_subprocess_end_in_hand() -> LinkDeliveryWithTheSubprocessEndInHand
        {
            let (parent_end, subprocess_end) = UnixStream::pair().expect("socketpair");
            subprocess_end
                .set_read_timeout(Some(Duration::from_millis(200)))
                .expect("a read timeout on the subprocess end");
            LinkDeliveryWithTheSubprocessEndInHand {
                delivery: SubprocessBridgeLinkDelivery {
                    processor_display_name: "BlurProcessor".to_string(),
                    parent_side: Arc::new(
                        ParentSideOfOneSubprocessBridgeSharedByItsThreads::over(
                            parent_end,
                            "Pblur".to_string(),
                        )
                        .expect("clone the parent end"),
                    ),
                },
                subprocess_end_reader: BufReader::new(subprocess_end),
            }
        }

        fn late_input_link() -> serde_json::Value {
            serde_json::json!({"link_id": "L-late", "name": "frames_from_upstream"})
        }

        #[test]
        fn a_link_handed_over_goes_out_as_a_wire_link_frame_and_its_answer_lands_on_its_cell() {
            let LinkDeliveryWithTheSubprocessEndInHand {
                delivery,
                mut subprocess_end_reader,
            } = link_delivery_with_the_subprocess_end_in_hand();

            let reply = OutOfProcessLinkWireReply::awaiting_the_far_sides_answer();
            delivery
                .hand_over_a_link_wired_after_setup(
                    PortDirection::Input,
                    &late_input_link(),
                    Arc::clone(&reply),
                )
                .expect("a live subprocess takes the link");

            assert_eq!(
                read_frame(&mut subprocess_end_reader).expect("the frame reached the subprocess"),
                serde_json::json!({
                    "cmd": "wire_link",
                    "direction": "input",
                    "link": late_input_link(),
                })
            );
            assert!(
                delivery
                    .parent_side
                    .links_awaiting_their_wire_reply
                    .note_the_far_sides_answer_for_link(
                        "L-late",
                        OutOfProcessLinkWireOutcome::OpenedByTheFarSide
                    ),
                "the cell is registered for the answer the reader routes"
            );
            assert_eq!(
                reply.the_far_sides_answer(),
                Some(OutOfProcessLinkWireOutcome::OpenedByTheFarSide)
            );
        }

        #[test]
        fn an_unwired_link_goes_out_unanswered_and_stops_waiting_on_its_answer() {
            let LinkDeliveryWithTheSubprocessEndInHand {
                delivery,
                mut subprocess_end_reader,
            } = link_delivery_with_the_subprocess_end_in_hand();
            delivery
                .hand_over_a_link_wired_after_setup(
                    PortDirection::Input,
                    &late_input_link(),
                    OutOfProcessLinkWireReply::awaiting_the_far_sides_answer(),
                )
                .expect("a live subprocess takes the link");
            read_frame(&mut subprocess_end_reader).expect("the wire_link frame");

            delivery
                .tell_the_far_side_a_link_was_unwired(
                    PortDirection::Input,
                    "frames_from_upstream",
                    "L-late",
                )
                .expect("a live subprocess is told");

            assert_eq!(
                read_frame(&mut subprocess_end_reader).expect("the frame reached the subprocess"),
                serde_json::json!({
                    "cmd": "unwire_link",
                    "direction": "input",
                    "port": "frames_from_upstream",
                    "link_id": "L-late",
                })
            );
            assert!(
                !delivery
                    .parent_side
                    .links_awaiting_their_wire_reply
                    .note_the_far_sides_answer_for_link(
                        "L-late",
                        OutOfProcessLinkWireOutcome::OpenedByTheFarSide
                    ),
                "a link on its way out is owed no answer"
            );
        }

        #[test]
        fn a_bridge_that_gave_up_refuses_a_link_and_tells_nobody_of_an_unwire() {
            let LinkDeliveryWithTheSubprocessEndInHand {
                delivery,
                mut subprocess_end_reader,
            } = link_delivery_with_the_subprocess_end_in_hand();
            delivery.parent_side.give_up_on_the_subprocess("is gone");

            let refused = delivery
                .hand_over_a_link_wired_after_setup(
                    PortDirection::Input,
                    &late_input_link(),
                    OutOfProcessLinkWireReply::awaiting_the_far_sides_answer(),
                )
                .expect_err("a subprocess the bridge gave up on opens no port");
            assert!(
                refused
                    .to_string()
                    .contains("'BlurProcessor' (Pblur) has failed"),
                "the refusal names the processor; got {refused}"
            );
            delivery
                .tell_the_far_side_a_link_was_unwired(
                    PortDirection::Input,
                    "frames_from_upstream",
                    "L-late",
                )
                .expect("a subprocess that is gone needs no telling");
            assert!(
                read_frame(&mut subprocess_end_reader).is_err(),
                "nothing was written to a subprocess the bridge gave up on"
            );
        }

        /// A helper its host gave up on while still alive refuses the links it
        /// still owed an answer for, so a later disconnect leaves nothing
        /// waiting and the helper's eventual death refuses nothing.
        ///
        /// Fail-without-fix: give up without refusing what the helper owed, and
        /// the link reads pending until the death refuses a link the graph no
        /// longer has.
        #[test]
        fn a_helper_given_up_on_refuses_what_it_owed_and_its_death_refuses_nothing_after() {
            let LinkDeliveryWithTheSubprocessEndInHand {
                delivery,
                mut subprocess_end_reader,
            } = link_delivery_with_the_subprocess_end_in_hand();
            let parent_side = Arc::clone(&delivery.parent_side);
            let envelope = OutOfProcessLinkWiringEnvelope::for_a_far_side_driven_in(
                ProcessExecution::Reactive,
            );
            envelope
                .send_the_setup_command_then_hand_every_later_link_over(|_| Ok(()), delivery)
                .expect("the setup command goes out");
            let answer_cell = envelope
                .record_a_link_and_hand_it_to_a_far_side_past_its_setup_command(
                    PortDirection::Input,
                    late_input_link(),
                )
                .expect("a live subprocess takes the link")
                .expect("a link handed over waits on its answer");
            read_frame(&mut subprocess_end_reader).expect("the wire_link frame");

            envelope.refuse_every_later_link_because_the_far_side_is_gone(
                refusal_of_a_link_into_a_helper_process_that_failed("BlurProcessor", "Pblur"),
            );
            assert!(
                matches!(
                    answer_cell.the_far_sides_answer(),
                    Some(OutOfProcessLinkWireOutcome::RefusedByTheFarSide { .. })
                ),
                "a link its helper still owed an answer for reads error once the host gives up"
            );

            envelope
                .forget_a_link_and_tell_a_far_side_past_its_setup_command(
                    PortDirection::Input,
                    "frames_from_upstream",
                    "L-late",
                )
                .expect("a far side given up on needs no telling");
            assert!(
                read_frame(&mut subprocess_end_reader).is_err(),
                "a helper given up on is told nothing"
            );
            let ((), warnings) = CapturedTracingWarnings::captured_while(|| {
                parent_side.give_up_on_the_subprocess("is gone")
            });
            assert!(
                warnings.is_empty(),
                "the helper's death refuses no link: {warnings:?}"
            );
        }
    }

    /// A link handed over while the helper is still inside `setup()` leaves the
    /// engine reading it as in setup, so the window that hook may mint is not
    /// refused. GPU-gated: a bridge needs a GPU capability to construct.
    ///
    /// Fail-without-fix: hand links over through `SubprocessBridge::send` and
    /// `wire_link` becomes the last lifecycle command the helper was sent.
    #[test]
    fn a_link_handed_over_during_setup_leaves_the_helper_read_as_in_setup() {
        const TEST: &str = "a_link_handed_over_during_setup_leaves_the_helper_read_as_in_setup";
        let Some(sandbox) = gpu_sandbox_or_skip(TEST) else {
            return;
        };
        let (parent_end, subprocess_end) = UnixStream::pair().expect("socketpair");
        let bridge = SubprocessBridge::new(
            parent_end,
            sandbox,
            a_mesh_link_ingress_table_carrying_nothing(),
            "p-setup-phase-test".into(),
        )
        .expect("bridge construction");

        bridge
            .send(&serde_json::json!({"cmd": SETUP_LIFECYCLE_COMMAND_TO_HELPER_PROCESS}))
            .expect("the setup command goes out");
        bridge
            .link_delivery_to_this_subprocess("SetupPhaseTestProcessor")
            .hand_over_a_link_wired_after_setup(
                PortDirection::Input,
                &serde_json::json!({"link_id": "L-during-setup", "name": "in1"}),
                OutOfProcessLinkWireReply::awaiting_the_far_sides_answer(),
            )
            .expect("the link is handed over");

        assert!(
            bridge
                .registry()
                .the_last_lifecycle_command_sent_to_the_helper_process_was_setup(),
            "a link handed over is not a lifecycle command"
        );
        drop(subprocess_end);
    }

    fn escalate_request_awaiting_an_answer(op: &str, request_id: &str) -> serde_json::Value {
        serde_json::json!({
            "rpc": "escalate_request",
            "op": op,
            "request_id": request_id,
        })
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
        let bridge = SubprocessBridge::new(
            parent_end,
            sandbox,
            a_mesh_link_ingress_table_carrying_nothing(),
            "p-bridge-test".into(),
        )
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
        let bridge = SubprocessBridge::new(
            parent_end,
            sandbox,
            a_mesh_link_ingress_table_carrying_nothing(),
            "p-bridge-test".into(),
        )
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
            let bridge = SubprocessBridge::new(
                parent_end,
                sandbox.clone(),
                a_mesh_link_ingress_table_carrying_nothing(),
                format!("p-crash-{cycle}"),
            )
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
