// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

mod lifecycle_replies_matched_to_their_commands;
mod subprocess_bridge_link_delivery;
mod subprocess_bridge_reader_and_escalate_workers;

use lifecycle_replies_matched_to_their_commands::LifecycleRepliesMatchedToTheirCommands;
pub(crate) use lifecycle_replies_matched_to_their_commands::{
    LifecycleCommandAwaitingItsReply, LifecycleCommandSent, LifecycleReplyNotReceived,
};
pub use subprocess_bridge_link_delivery::SubprocessBridgeLinkDelivery;

use std::io::{BufReader, BufWriter, Read, Write};
use std::os::unix::io::{AsRawFd, RawFd};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crate::core::context::GpuContextLimitedAccess;
use crate::core::error::{Error, Result};
use crate::core::logging::carrying_this_threads_project_log_route_entry_and_reading_a_helper_pipe;
use crate::core::nodes::{
    LinksAwaitingTheirOutOfProcessWireReply, OutOfProcessCommandId, OutOfProcessLinkWireOutcome,
};

use super::subprocess_escalate::handle_lifecycle::{
    EscalateHandleRegistry, release_everything_a_torn_down_helper_held,
};
use super::subprocess_escalate::{
    ESCALATE_OP_ANSWERED_BY_NOTHING, ESCALATE_OP_BUILDING_A_KERNEL, WhatAHelperPlacedNodeReaches,
    process_bridge_message, shared_native_service_lease::SharedNativeServiceLeasesOfOneHelper,
};
use subprocess_bridge_reader_and_escalate_workers::{
    SubprocessBridgeThreads, node_run_id_shortened_for_a_thread_name,
    refuse_every_link_this_subprocess_still_owed, spawn_the_native_escalate_worker,
    spawn_the_reader_and_its_two_escalate_workers,
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
/// The helper borrows its parent's own `tatolab.runtime` through the lend
/// directory, so the ids match by construction; the check is the backstop for
/// a helper that imported another build anyway, which would otherwise surface
/// as every service open failing on a corrupted service.
pub(crate) const ENGINE_BUILD_ID_ENVIRONMENT_VARIABLE: &str = "STREAMLIB_ENGINE_BUILD_ID";

/// Env var advertising the inherited child-end fd number of the native
/// escalate socketpair: the channel a helper's native code sends escalate
/// requests over without Python, answered in order, one at a time.
pub(crate) const NATIVE_ESCALATE_FD_ENV: &str = "STREAMLIB_NATIVE_ESCALATE_FD";

/// Socketpair-backed escalate IPC transport: the bridge socket the subprocess
/// inherits via [`ESCALATE_FD_ENV`], and the native escalate socket it
/// inherits via [`NATIVE_ESCALATE_FD_ENV`].
pub(crate) struct EscalateTransport {
    parent_end: UnixStream,
    native_escalate_parent_end: UnixStream,
    /// Kept alive so the child fds stay open across `Command::spawn`. The
    /// caller drops them after spawn so only the subprocess holds the child
    /// ends.
    child_ends: Option<(UnixStream, UnixStream)>,
}

/// The parent's halves of an [`EscalateTransport`].
pub(crate) struct EscalateTransportParentEnds {
    /// Lifecycle commands, log records and the helper's Python escalates.
    pub bridge: UnixStream,
    /// Escalate requests from the helper's native code.
    pub native_escalate: UnixStream,
}

impl EscalateTransport {
    /// Create both socketpairs, register `pre_exec` on `command` to clear
    /// `FD_CLOEXEC` on each child-end fd, and name them in the command's
    /// environment.
    ///
    /// After `command.spawn()`, call [`Self::release_child_end`] so only
    /// the subprocess retains the child-side fds.
    pub(crate) fn attach(command: &mut Command) -> Result<Self> {
        let (parent_end, child_end) = UnixStream::pair()
            .map_err(|e| Error::Runtime(format!("failed to create escalate socketpair: {e}")))?;
        let (native_escalate_parent_end, native_escalate_child_end) =
            UnixStream::pair().map_err(|e| {
                Error::Runtime(format!("failed to create native escalate socketpair: {e}"))
            })?;

        let child_fds: [RawFd; 2] = [child_end.as_raw_fd(), native_escalate_child_end.as_raw_fd()];

        // Clear FD_CLOEXEC on the child-end fds between fork and exec so
        // the execed subprocess inherits them. `fcntl` is async-signal-safe
        // so it's legal to call from `pre_exec`.
        unsafe {
            command.pre_exec(move || {
                for child_fd in child_fds {
                    let flags = libc::fcntl(child_fd, libc::F_GETFD);
                    if flags < 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                    let rc = libc::fcntl(child_fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC);
                    if rc < 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                }
                Ok(())
            });
        }

        command.env(ESCALATE_FD_ENV, child_fds[0].to_string());
        command.env(NATIVE_ESCALATE_FD_ENV, child_fds[1].to_string());

        Ok(Self {
            parent_end,
            native_escalate_parent_end,
            child_ends: Some((child_end, native_escalate_child_end)),
        })
    }

    /// Drop the parent's references to the child-end fds. Call this after
    /// `command.spawn()` succeeds so only the subprocess keeps them open.
    pub(crate) fn release_child_end(&mut self) {
        self.child_ends.take();
    }

    /// Consume the transport and return the parent-side sockets.
    pub(crate) fn into_parent_ends(mut self) -> EscalateTransportParentEnds {
        self.child_ends.take();
        EscalateTransportParentEnds {
            bridge: self.parent_end,
            native_escalate: self.native_escalate_parent_end,
        }
    }
}

/// The lifecycle command that runs a helper's `setup()`.
pub(crate) const SETUP_LIFECYCLE_COMMAND_TO_HELPER_PROCESS: &str = "setup";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HelperProcessShutdownCommand {
    Stop,
    Teardown,
}

impl HelperProcessShutdownCommand {
    /// Both commands, in the order the shutdown ladder sends them.
    ///
    /// `stop` and `teardown` are sent together.
    pub(crate) const BOTH_IN_THE_ORDER_THE_LADDER_SENDS_THEM: [Self; 2] =
        [Self::Stop, Self::Teardown];

    /// The lifecycle command it is sent as.
    pub(crate) fn lifecycle_command(self) -> LifecycleCommandAwaitingItsReply {
        match self {
            Self::Stop => LifecycleCommandAwaitingItsReply::Stop,
            Self::Teardown => LifecycleCommandAwaitingItsReply::Teardown,
        }
    }
}

/// The field a command the parent sends that expects an answer carries its
/// [`OutOfProcessCommandId`] in, and the field the helper echoes it back in.
pub(crate) const OUT_OF_PROCESS_COMMAND_ID_FIELD: &str = "command_id";

/// The [`OutOfProcessCommandId`] a frame from the helper echoes, if it carries one.
pub(super) fn out_of_process_command_id_of(
    frame: &serde_json::Value,
) -> Option<OutOfProcessCommandId> {
    frame
        .get(OUT_OF_PROCESS_COMMAND_ID_FIELD)
        .and_then(|command_id| command_id.as_u64())
        .map(OutOfProcessCommandId)
}

/// The rpc tag a subprocess answers a late `wire_link` with once it has opened
/// its own port for the link. Carries `command_id` and `link_id`.
pub(crate) const OUT_OF_PROCESS_LINK_WIRED_RPC: &str = "link_wired";

/// The rpc tag a subprocess answers a late `wire_link` with when it could not
/// open its port. Carries `command_id`, `link_id` and `reason`.
pub(crate) const OUT_OF_PROCESS_LINK_WIRE_FAILED_RPC: &str = "link_wire_failed";

/// How many escalate requests one helper may have queued for one of its workers
/// before the next is refused.
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
/// through to the lifecycle queue by accident ends the wait of whichever
/// lifecycle command the host is waiting on, as a reply naming no command.
#[derive(Debug, PartialEq, Eq)]
enum IncomingSubprocessFrame {
    /// An escalate request the helper waits on no answer for — a log record —
    /// dispatched on the reader thread the moment it arrives.
    EscalateRequestAnsweredByNothing,
    /// An escalate request the helper waits on an answer for, queued for the
    /// helper's escalate worker.
    EscalateRequestAwaitingAnAnswer,
    /// A kernel build the helper waits on an answer for, queued for the
    /// helper's kernel build worker, so the frames' dispatches queued behind it
    /// on the escalate worker never wait out its compile.
    EscalateRequestBuildingAKernel,
    /// The answer to one `wire_link` or `feed_island` the host sent after setup.
    LinkWireAnswer {
        command_id: OutOfProcessCommandId,
        link_id: String,
        outcome: OutOfProcessLinkWireOutcome,
    },
    /// A link's answer that names no command or no link, so it answers no
    /// attempt.
    LinkWireAnswerNamingNoCommandOrNoLink,
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
            Some(ESCALATE_OP_BUILDING_A_KERNEL) => {
                IncomingSubprocessFrame::EscalateRequestBuildingAKernel
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
    match (
        out_of_process_command_id_of(msg),
        msg.get("link_id").and_then(|link_id| link_id.as_str()),
    ) {
        (Some(command_id), Some(link_id)) => IncomingSubprocessFrame::LinkWireAnswer {
            command_id,
            link_id: link_id.to_string(),
            outcome,
        },
        _ => IncomingSubprocessFrame::LinkWireAnswerNamingNoCommandOrNoLink,
    }
}

/// What a link wired into a helper process that has failed is refused with.
pub(crate) fn refusal_of_a_link_into_a_helper_process_that_failed(
    node_display_name: &str,
    node_run_id: &str,
) -> String {
    format!(
        "node '{node_display_name}' ({node_run_id}) has failed, so no link can be \
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
    node_run_id: String,
    framed_socket_writer: parking_lot::Mutex<BufWriter<UnixStream>>,
    /// Shut down outside the writer's lock, so teardown never waits behind a
    /// write the subprocess is not reading.
    socket_for_shutting_down: UnixStream,
    the_subprocess_was_given_up_on: AtomicBool,
    links_awaiting_their_wire_reply: LinksAwaitingTheirOutOfProcessWireReply,
    /// The id the next command expecting an answer goes out with, shared by
    /// lifecycle commands and link wiring so one id never names two commands.
    next_out_of_process_command_id: AtomicU64,
}

impl ParentSideOfOneSubprocessBridgeSharedByItsThreads {
    fn over(parent_end: UnixStream, node_run_id: String) -> Result<Self> {
        let socket_for_shutting_down = parent_end.try_clone().map_err(|e| {
            Error::Runtime(format!(
                "failed to clone escalate socketpair for shutdown: {e}"
            ))
        })?;
        Ok(Self {
            node_run_id,
            framed_socket_writer: parking_lot::Mutex::new(BufWriter::new(parent_end)),
            socket_for_shutting_down,
            the_subprocess_was_given_up_on: AtomicBool::new(false),
            links_awaiting_their_wire_reply: LinksAwaitingTheirOutOfProcessWireReply::default(),
            next_out_of_process_command_id: AtomicU64::new(1),
        })
    }

    fn mint_an_out_of_process_command_id(&self) -> OutOfProcessCommandId {
        OutOfProcessCommandId(
            self.next_out_of_process_command_id
                .fetch_add(1, Ordering::Relaxed),
        )
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
            &self.node_run_id,
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
/// socket is shut down, the reader sees EOF, and the escalate worker and the
/// kernel build worker leave once the reader has.
pub struct SubprocessBridge {
    parent_side: Arc<ParentSideOfOneSubprocessBridgeSharedByItsThreads>,
    lifecycle_replies: LifecycleRepliesMatchedToTheirCommands,
    registry: Arc<EscalateHandleRegistry>,
    /// Held for teardown: the drop path evicts what the registry's acquires
    /// entered into the parent's texture cache, which needs the same
    /// capability the escalate worker dispatches against.
    sandbox: GpuContextLimitedAccess,
    /// The helper's shared native service leases, given back with the bridge.
    shared_native_service_leases: Arc<SharedNativeServiceLeasesOfOneHelper>,
    frame_demultiplexing_reader_thread: Option<JoinHandle<()>>,
    escalate_worker_thread: Option<JoinHandle<()>>,
    kernel_build_worker_thread: Option<JoinHandle<()>>,
    /// The native escalate socket, shut down with the bridge so its worker
    /// sees EOF.
    native_escalate_socket_for_shutting_down: Option<UnixStream>,
    native_escalate_worker_thread: Option<JoinHandle<()>>,
}

impl SubprocessBridge {
    /// [`Self::new`] for a helper whose node carries no catalog entries, for a
    /// caller outside the engine that wires a bridge over its own socketpair.
    pub fn for_a_helper_whose_node_carries_no_catalog_entries(
        stream: UnixStream,
        sandbox: GpuContextLimitedAccess,
        node_run_id: String,
    ) -> Result<Self> {
        Self::new(
            stream,
            None,
            sandbox,
            node_run_id,
            WhatAHelperPlacedNodeReaches::default(),
        )
    }

    /// Wrap a socketpair parent end and spawn the reader thread, the escalate
    /// worker and the kernel build worker, which dispatch against `sandbox` and
    /// what the helper's node reaches; with `native_escalate_stream`, a second worker
    /// answers the helper's native code over it, against the same registry.
    pub(crate) fn new(
        stream: UnixStream,
        native_escalate_stream: Option<UnixStream>,
        sandbox: GpuContextLimitedAccess,
        node_run_id: String,
        what_the_node_reaches: WhatAHelperPlacedNodeReaches,
    ) -> Result<Self> {
        let shared_native_service_leases =
            Arc::clone(&what_the_node_reaches.shared_native_service_leases);
        let read_half = stream.try_clone().map_err(|e| {
            Error::Runtime(format!(
                "failed to clone escalate socketpair for reader: {e}"
            ))
        })?;
        let parent_side = Arc::new(ParentSideOfOneSubprocessBridgeSharedByItsThreads::over(
            stream,
            node_run_id,
        )?);
        let registry = EscalateHandleRegistry::for_a_helper_whose_output_links_keep(Arc::clone(
            &what_the_node_reaches.output_frames_kept_by_its_output_links,
        ));
        let (lifecycle_tx, lifecycle_rx) = mpsc::channel();

        let dispatch_registry = Arc::clone(&registry);
        let dispatch_sandbox = sandbox.clone();
        let escalate_request_dispatch: EscalateRequestDispatch = Arc::new(move |frame| {
            process_bridge_message(
                &dispatch_sandbox,
                &dispatch_registry,
                &what_the_node_reaches,
                frame,
            )
        });
        let SubprocessBridgeThreads {
            frame_demultiplexing_reader_thread,
            escalate_worker_thread,
            kernel_build_worker_thread,
        } = spawn_the_reader_and_its_two_escalate_workers(
            BufReader::new(read_half),
            Arc::clone(&parent_side),
            Arc::clone(&escalate_request_dispatch),
            lifecycle_tx,
        )?;
        let (native_escalate_socket_for_shutting_down, native_escalate_worker_thread) =
            match native_escalate_stream {
                Some(native_escalate_stream) => {
                    let socket_for_shutting_down =
                        native_escalate_stream.try_clone().map_err(|e| {
                            Error::Runtime(format!(
                                "failed to clone the native escalate socket for shutdown: {e}"
                            ))
                        })?;
                    let worker = spawn_the_native_escalate_worker(
                        native_escalate_stream,
                        Arc::clone(&parent_side),
                        escalate_request_dispatch,
                    )?;
                    (Some(socket_for_shutting_down), Some(worker))
                }
                None => (None, None),
            };

        Ok(Self {
            parent_side,
            lifecycle_replies: LifecycleRepliesMatchedToTheirCommands::arriving_on(lifecycle_rx),
            registry,
            sandbox,
            shared_native_service_leases,
            frame_demultiplexing_reader_thread: Some(frame_demultiplexing_reader_thread),
            escalate_worker_thread: Some(escalate_worker_thread),
            kernel_build_worker_thread: Some(kernel_build_worker_thread),
            native_escalate_socket_for_shutting_down,
            native_escalate_worker_thread,
        })
    }

    /// Write a lifecycle command the subprocess answers nothing to, such as
    /// `run`. Link wiring goes through [`SubprocessBridgeLinkDelivery`] instead.
    pub fn send_a_command_answered_by_nothing(&self, msg: &serde_json::Value) -> Result<()> {
        self.write_a_lifecycle_command(msg)
    }

    /// Write `command` with the fields of `frame`, stamped with the id its reply
    /// is matched to.
    pub(crate) fn send_a_lifecycle_command_awaiting_its_reply(
        &self,
        command: LifecycleCommandAwaitingItsReply,
        mut frame: serde_json::Value,
    ) -> Result<LifecycleCommandSent> {
        let sent = LifecycleCommandSent {
            command,
            command_id: self.parent_side.mint_an_out_of_process_command_id(),
        };
        frame["cmd"] = serde_json::json!(command.command_tag());
        frame[OUT_OF_PROCESS_COMMAND_ID_FIELD] = serde_json::json!(sent.command_id.0);
        // Owed before the write: the subprocess can answer the moment the
        // frame lands.
        self.lifecycle_replies.owe_a_reply_to(sent);
        if let Err(not_sent) = self.write_a_lifecycle_command(&frame) {
            self.lifecycle_replies.owe_no_reply_to(sent);
            return Err(not_sent);
        }
        Ok(sent)
    }

    /// Wait up to `timeout` for the reply carrying `awaited`'s id.
    pub(crate) fn await_the_reply_to(
        &self,
        awaited: LifecycleCommandSent,
        timeout: Duration,
    ) -> std::result::Result<serde_json::Value, LifecycleReplyNotReceived> {
        self.lifecycle_replies
            .await_the_reply_to(awaited, timeout, &self.parent_side.node_run_id)
    }

    /// Stop waiting on the reply to `sent`, so one that arrives later is
    /// dropped rather than read as the answer to another command.
    pub(crate) fn abandon_the_reply_to(&self, sent: LifecycleCommandSent) {
        self.lifecycle_replies.abandon_the_reply_to(sent);
    }

    /// Stop waiting on the reply to every lifecycle command still owed one.
    pub(crate) fn abandon_the_reply_to_every_lifecycle_command_still_owed_one(&self) {
        self.lifecycle_replies
            .abandon_the_reply_to_every_command_still_owed_one();
    }

    fn write_a_lifecycle_command(&self, msg: &serde_json::Value) -> Result<()> {
        if self.parent_side.the_subprocess_was_given_up_on() {
            return Err(Error::Runtime(format!(
                "[{}] bridge marked dead, cannot send",
                self.parent_side.node_run_id
            )));
        }
        self.parent_side
            .write_frame_or_give_up_on_the_subprocess(msg)
    }

    /// Mark the bridge dead; subsequent sends return immediately, and every
    /// link this subprocess was still to answer for is refused.
    pub fn mark_dead(&self) {
        self.parent_side.give_up_on_the_subprocess("is gone");
    }

    pub fn is_dead(&self) -> bool {
        self.parent_side.the_subprocess_was_given_up_on()
    }

    /// The run id of the node whose subprocess this bridge reaches.
    pub(crate) fn node_run_id(&self) -> &str {
        &self.parent_side.node_run_id
    }

    /// How a link wired after this subprocess's setup command reaches it, for
    /// its wiring envelope to hand links over through.
    pub fn link_delivery_to_this_subprocess(
        &self,
        node_display_name: &str,
    ) -> SubprocessBridgeLinkDelivery {
        SubprocessBridgeLinkDelivery {
            node_display_name: node_display_name.to_string(),
            parent_side: Arc::clone(&self.parent_side),
        }
    }
}

impl Drop for SubprocessBridge {
    fn drop(&mut self) {
        self.mark_dead();
        // Given up on and shut down before draining: both workers pass over
        // every request still queued, and the reader queues nothing past EOF,
        // because an acquire landing after the drain would strand its cache
        // entry. A request the escalate worker is already executing can still
        // slip through; closing that too would mean joining the worker, which
        // this path deliberately never blocks on. A build already executing on
        // the kernel build worker releases its own kernel once it sees the helper
        // given up on; the native escalate worker answers builds inline and does not.
        self.parent_side.shut_the_socket_down();
        if let Some(native_escalate_socket) = &self.native_escalate_socket_for_shutting_down {
            let _ = native_escalate_socket.shutdown(std::net::Shutdown::Both);
        }
        // Run the release path's kind-specific cleanup for everything the
        // helper never released — a crashed child must not strand cache
        // entries in a GpuContext that outlives every respawn, nor
        // surface-share registrations and their fd dups: the host's own
        // connection registered those on the helper's behalf, so the
        // service's disconnect watchdog rightly never reclaims them.
        release_everything_a_torn_down_helper_held(&self.sandbox, &self.registry);
        self.shared_native_service_leases.release_every_lease();
        // Detached, never joined: the OS reaps both on process exit.
        self.frame_demultiplexing_reader_thread.take();
        self.escalate_worker_thread.take();
        self.kernel_build_worker_thread.take();
        self.native_escalate_worker_thread.take();
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
/// thread-local for `init_for_tests`), and carries the caller's stream log
/// route, so each line lands in the log of the stream whose helper wrote it —
/// the stream's log stays open until the reader reaches the pipe's end.
pub(crate) fn spawn_fd_line_reader<R>(
    reader: R,
    thread_prefix: &str,
    channel: &'static str,
    node_run_id: &str,
) -> Option<JoinHandle<()>>
where
    R: Read + Send + 'static,
{
    let proc_id = node_run_id.to_string();
    let name = format!(
        "{}-{}",
        thread_prefix,
        node_run_id_shortened_for_a_thread_name(node_run_id)
    );
    let (source, target): (&'static str, &'static str) = if thread_prefix.starts_with("py") {
        ("python", "streamlib::polyglot::python")
    } else {
        ("deno", "streamlib::polyglot::deno")
    };
    let dispatch = tracing::dispatcher::get_default(|d| d.clone());

    thread::Builder::new()
        .name(name)
        .spawn(
            carrying_this_threads_project_log_route_entry_and_reading_a_helper_pipe(move || {
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
            }),
        )
        .ok()
}

fn emit_intercepted_line(
    target: &'static str,
    channel: &'static str,
    source: &'static str,
    node_run_id: &str,
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
            node_run_id = %node_run_id,
            "{}",
            text
        ),
        _ => tracing::warn!(
            target: "streamlib::polyglot::deno",
            intercepted = true,
            channel = channel,
            source = source,
            node_run_id = %node_run_id,
            "{}",
            text
        ),
    }
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
mod tests;
