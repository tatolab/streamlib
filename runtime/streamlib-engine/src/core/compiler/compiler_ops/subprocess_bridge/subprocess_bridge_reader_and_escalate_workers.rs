// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The bridge's reader thread and the escalate workers it feeds.

use std::io::{BufReader, BufWriter};
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver};
use std::thread::{self, JoinHandle};

use crate::core::error::{Error, Result};
use crate::core::logging::carrying_this_threads_project_log_route_entry_and_reading_a_helper_pipe;
use crate::core::nodes::{
    LinksAwaitingTheirOutOfProcessWireReply, OutOfProcessCommandId, OutOfProcessLinkWireAnswerNoted,
};

use crate::core::compiler::compiler_ops::subprocess_escalate::{
    envelope_request, handle_id_of_an_enveloped_ok_response, refusal_of_an_escalate_request,
};
use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::EscalateRequest;
use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::escalate_request::EscalateRequestReleaseHandle;

use super::{
    ESCALATE_REQUESTS_QUEUED_PER_HELPER, EscalateRequestDispatch, IncomingSubprocessFrame,
    ParentSideOfOneSubprocessBridgeSharedByItsThreads, classify_an_incoming_subprocess_frame,
    read_frame, write_frame,
};

/// The threads one bridge always runs; the native escalate worker is started
/// apart, only for a helper that has native code.
pub(super) struct SubprocessBridgeThreads {
    pub(super) frame_demultiplexing_reader_thread: JoinHandle<()>,
    pub(super) escalate_worker_thread: JoinHandle<()>,
    pub(super) kernel_build_worker_thread: JoinHandle<()>,
}

/// One worker's queue as the reader feeds it, with the name a refusal or a
/// warning calls that worker by.
struct QueueToOneWorker {
    tx: mpsc::SyncSender<serde_json::Value>,
    worker_named_in_a_refusal: &'static str,
}

/// Spawn the escalate worker and the kernel build worker, then the reader that
/// feeds them.
///
/// The reader owns the only sender of each worker's queue, so each worker
/// leaves once the reader has and every request queued for it is taken off.
pub(super) fn spawn_the_reader_and_its_two_escalate_workers(
    reader: BufReader<UnixStream>,
    parent_side: Arc<ParentSideOfOneSubprocessBridgeSharedByItsThreads>,
    escalate_request_dispatch: EscalateRequestDispatch,
    lifecycle_tx: mpsc::Sender<serde_json::Value>,
) -> Result<SubprocessBridgeThreads> {
    let (escalate_requests_tx, escalate_requests_rx) =
        mpsc::sync_channel(ESCALATE_REQUESTS_QUEUED_PER_HELPER);
    let (kernel_builds_tx, kernel_builds_rx) =
        mpsc::sync_channel(ESCALATE_REQUESTS_QUEUED_PER_HELPER);
    let node_run_id = parent_side.node_run_id.clone();

    let escalate_worker_thread = spawn_a_worker_answering_each_queued_request_in_order(
        "br-esc",
        "escalate worker",
        escalate_requests_rx,
        Arc::clone(&parent_side),
        Arc::clone(&escalate_request_dispatch),
        AnAnswerMadeAfterTheGiveUp::IsWrittenLikeAnyOther,
    )?;
    let kernel_build_worker_thread = spawn_a_worker_answering_each_queued_request_in_order(
        "br-bld",
        "kernel build worker",
        kernel_builds_rx,
        Arc::clone(&parent_side),
        Arc::clone(&escalate_request_dispatch),
        AnAnswerMadeAfterTheGiveUp::ReleasesTheKernelItRegistered,
    )?;
    let frame_demultiplexing_reader_thread =
        spawn_a_bridge_thread("br", &node_run_id, "reader", move || {
            reader_loop(
                reader,
                &parent_side,
                &escalate_request_dispatch,
                QueueToOneWorker {
                    tx: escalate_requests_tx,
                    worker_named_in_a_refusal: "escalate worker",
                },
                QueueToOneWorker {
                    tx: kernel_builds_tx,
                    worker_named_in_a_refusal: "kernel build worker",
                },
                lifecycle_tx,
            );
        })?;

    Ok(SubprocessBridgeThreads {
        frame_demultiplexing_reader_thread,
        escalate_worker_thread,
        kernel_build_worker_thread,
    })
}

/// Start one named bridge thread carrying the caller's log route, its name
/// `<thread_name_prefix>-<node run id, shortened>`.
fn spawn_a_bridge_thread(
    thread_name_prefix: &str,
    node_run_id: &str,
    what_the_thread_is: &str,
    body: impl FnOnce() + Send + 'static,
) -> Result<JoinHandle<()>> {
    thread::Builder::new()
        .name(format!(
            "{thread_name_prefix}-{}",
            node_run_id_shortened_for_a_thread_name(node_run_id)
        ))
        .spawn(carrying_this_threads_project_log_route_entry_and_reading_a_helper_pipe(body))
        .map_err(|spawn_failure| {
            Error::Runtime(format!(
                "[{node_run_id}] could not start its bridge's {what_the_thread_is}: \
                 {spawn_failure}"
            ))
        })
}

/// Reader loop: drain the parent-side socket and route every frame, never
/// waiting on the work a frame asks for.
fn reader_loop(
    mut reader: BufReader<UnixStream>,
    parent_side: &ParentSideOfOneSubprocessBridgeSharedByItsThreads,
    escalate_request_dispatch: &EscalateRequestDispatch,
    escalate_worker_queue: QueueToOneWorker,
    kernel_build_worker_queue: QueueToOneWorker,
    lifecycle_tx: mpsc::Sender<serde_json::Value>,
) {
    let node_run_id = &parent_side.node_run_id;
    loop {
        let msg = match read_frame(&mut reader) {
            Ok(v) => v,
            Err(e) => {
                tracing::debug!("[{}] bridge reader exiting: {}", node_run_id, e);
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
            IncomingSubprocessFrame::LinkWireAnswer {
                command_id,
                link_id,
                outcome,
            } => {
                log_an_answer_that_settled_no_link_wire_attempt(
                    node_run_id,
                    command_id,
                    &link_id,
                    parent_side
                        .links_awaiting_their_wire_reply
                        .note_the_far_sides_answer_to(command_id, &link_id, outcome),
                );
                continue;
            }
            IncomingSubprocessFrame::LinkWireAnswerNamingNoCommand => {
                tracing::warn!(
                    "[{node_run_id}] its helper process answered a wire naming no command, so it \
                     settles no link"
                );
                continue;
            }
            IncomingSubprocessFrame::LinkWireAnswerNamingNoLink => {
                tracing::warn!(
                    "[{node_run_id}] its helper process answered a wire naming no link, so it \
                     settles no link"
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
                if !escalate_worker_queue.queue_or_refuse(msg, parent_side) {
                    break;
                }
                continue;
            }
            IncomingSubprocessFrame::EscalateRequestBuildingAKernel => {
                if !kernel_build_worker_queue.queue_or_refuse(msg, parent_side) {
                    break;
                }
                continue;
            }
        }

        // Lifecycle response — forward to main thread. Send failure
        // means the receiver is gone (host dropped), exit cleanly.
        if lifecycle_tx.send(msg).is_err() {
            tracing::debug!(
                "[{}] bridge reader exiting: lifecycle channel dropped",
                node_run_id
            );
            break;
        }
    }

    // Every way out of the loop above is this reader giving up on the
    // subprocess, so it is said once here rather than on each `break`.
    parent_side.give_up_on_the_subprocess("stopped answering");
}

impl QueueToOneWorker {
    /// Queue `request`, refusing it with an answer when the queue is full;
    /// `false` once the helper is given up on.
    fn queue_or_refuse(
        &self,
        request: serde_json::Value,
        parent_side: &ParentSideOfOneSubprocessBridgeSharedByItsThreads,
    ) -> bool {
        let worker = self.worker_named_in_a_refusal;
        match self.tx.try_send(request) {
            Ok(()) => true,
            // Refused rather than waited for: a reader blocked on a full queue
            // would hold this helper's log records and lifecycle replies behind
            // its GPU work again.
            Err(mpsc::TrySendError::Full(refused_request)) => {
                let refusal = refusal_of_an_escalate_request(
                    &refused_request,
                    format!(
                        "the app process already holds {ESCALATE_REQUESTS_QUEUED_PER_HELPER} \
                         requests from this helper process for its {worker} that it has not \
                         answered, so this one was refused rather than queued"
                    ),
                );
                parent_side
                    .write_frame_or_give_up_on_the_subprocess(&refusal)
                    .is_ok()
            }
            // A worker leaves before this sender is dropped only by panicking,
            // and a helper whose requests nothing can answer is given up on.
            Err(mpsc::TrySendError::Disconnected(_)) => {
                tracing::warn!(
                    "[{}] its bridge's {worker} is gone, so no request queued for it can be \
                     answered; giving up on the helper process",
                    parent_side.node_run_id
                );
                false
            }
        }
    }
}

/// What a worker does with an answer it made once the helper was given up on.
#[derive(Clone, Copy)]
enum AnAnswerMadeAfterTheGiveUp {
    /// Written like any other; on the escalate worker an acquire still running
    /// at the give-up can slip past teardown's drain, which `Drop` accepts.
    IsWrittenLikeAnyOther,
    /// Dropped, and the kernel it registered released through the helper's
    /// handle registry: teardown's drain may already have run, and the
    /// registry's counted release never lets one go twice.
    ReleasesTheKernelItRegistered,
}

/// Start a worker that dispatches each request queued for it in the order the
/// helper sent it and writes its answer, one at a time.
///
/// The escalate worker keeps a release behind the acquire it releases. Kernel
/// builds get a worker of their own, so a frame's dispatch never waits out a
/// compile; nothing the helper sends depends on a build it has no answer for.
///
/// A request still queued once the subprocess was given up on is passed over:
/// teardown's drain has released or will release what the helper held, and an
/// acquire dispatched after it would strand what it acquired.
fn spawn_a_worker_answering_each_queued_request_in_order(
    thread_name_prefix: &str,
    what_the_thread_is: &'static str,
    queued_requests_rx: Receiver<serde_json::Value>,
    parent_side: Arc<ParentSideOfOneSubprocessBridgeSharedByItsThreads>,
    escalate_request_dispatch: EscalateRequestDispatch,
    an_answer_made_after_the_give_up: AnAnswerMadeAfterTheGiveUp,
) -> Result<JoinHandle<()>> {
    let node_run_id = parent_side.node_run_id.clone();
    spawn_a_bridge_thread(
        thread_name_prefix,
        &node_run_id,
        what_the_thread_is,
        move || {
            for request in queued_requests_rx {
                if parent_side.the_subprocess_was_given_up_on() {
                    continue;
                }
                let Some(answer) = escalate_request_dispatch(&request) else {
                    continue;
                };
                if matches!(
                    an_answer_made_after_the_give_up,
                    AnAnswerMadeAfterTheGiveUp::ReleasesTheKernelItRegistered
                ) && parent_side.the_subprocess_was_given_up_on()
                {
                    release_the_kernel_an_unanswered_build_registered(
                        &answer,
                        &parent_side,
                        &escalate_request_dispatch,
                    );
                    continue;
                }
                if let Err(write_failure) =
                    parent_side.write_frame_or_give_up_on_the_subprocess(&answer)
                {
                    tracing::warn!(
                        "[{}] its bridge's {what_the_thread_is} failed to write an answer: {}",
                        parent_side.node_run_id,
                        write_failure
                    );
                }
            }
        },
    )
}

/// Release the kernel a build's `answer` names, if it registered one, through
/// the same dispatch the helper's own `release_handle` takes.
fn release_the_kernel_an_unanswered_build_registered(
    answer: &serde_json::Value,
    parent_side: &ParentSideOfOneSubprocessBridgeSharedByItsThreads,
    escalate_request_dispatch: &EscalateRequestDispatch,
) {
    let Some(kernel_id) = handle_id_of_an_enveloped_ok_response(answer) else {
        return;
    };
    let released = escalate_request_dispatch(&envelope_request(EscalateRequest::ReleaseHandle(
        EscalateRequestReleaseHandle {
            handle_id: kernel_id.clone(),
            request_id: "released-after-the-helper-was-given-up-on".to_string(),
        },
    )));
    tracing::debug!(
        "[{}] a kernel build finished after its helper process was given up on, so the \
         kernel {} it registered was released rather than answered: {:?}",
        parent_side.node_run_id,
        kernel_id,
        released
    );
}

/// Start the worker that answers the helper's native code over its own socket.
pub(super) fn spawn_the_native_escalate_worker(
    native_escalate_stream: UnixStream,
    parent_side: Arc<ParentSideOfOneSubprocessBridgeSharedByItsThreads>,
    escalate_request_dispatch: EscalateRequestDispatch,
) -> Result<JoinHandle<()>> {
    let node_run_id = parent_side.node_run_id.clone();
    spawn_a_bridge_thread(
        "br-nat",
        &node_run_id,
        "native escalate worker",
        move || {
            native_escalate_worker_loop(
                native_escalate_stream,
                &parent_side,
                &escalate_request_dispatch,
            );
        },
    )
}

/// Answer each escalate request the helper's native code sends, in arrival
/// order, until the socket closes or the subprocess is given up on.
///
/// Only escalate requests travel here; any other frame is refused with an
/// answer, so a native caller never waits on one that cannot come.
fn native_escalate_worker_loop(
    native_escalate_stream: UnixStream,
    parent_side: &ParentSideOfOneSubprocessBridgeSharedByItsThreads,
    escalate_request_dispatch: &EscalateRequestDispatch,
) {
    let node_run_id = &parent_side.node_run_id;
    let read_half = match native_escalate_stream.try_clone() {
        Ok(read_half) => read_half,
        Err(clone_failure) => {
            tracing::warn!(
                "[{node_run_id}] native escalate worker could not clone its socket: \
                 {clone_failure}"
            );
            return;
        }
    };
    let mut reader = BufReader::new(read_half);
    let mut writer = BufWriter::new(native_escalate_stream);
    loop {
        let request = match read_frame(&mut reader) {
            Ok(request) => request,
            Err(read_failure) => {
                tracing::debug!("[{node_run_id}] native escalate worker exiting: {read_failure}");
                return;
            }
        };
        if parent_side.the_subprocess_was_given_up_on() {
            return;
        }
        let answer = match classify_an_incoming_subprocess_frame(&request) {
            IncomingSubprocessFrame::EscalateRequestAwaitingAnAnswer
            | IncomingSubprocessFrame::EscalateRequestBuildingAKernel => {
                escalate_request_dispatch(&request)
            }
            _ => Some(refusal_of_an_escalate_request(
                &request,
                "the native escalate socket carries escalate requests that wait on an answer, \
                 and nothing else"
                    .to_string(),
            )),
        };
        let Some(answer) = answer else {
            continue;
        };
        if let Err(write_failure) = write_frame(&mut writer, &answer) {
            tracing::warn!(
                "[{node_run_id}] native escalate worker failed to write an answer: \
                 {write_failure}"
            );
            return;
        }
    }
}

/// Say why an answer to a `wire_link` or `feed_island` settled nothing; an
/// answer that settled its attempt says nothing.
fn log_an_answer_that_settled_no_link_wire_attempt(
    node_run_id: &str,
    command_id: OutOfProcessCommandId,
    link_id: &str,
    noted: OutOfProcessLinkWireAnswerNoted,
) {
    match noted {
        OutOfProcessLinkWireAnswerNoted::SettledItsAttempt => {}
        OutOfProcessLinkWireAnswerNoted::DroppedAsTheAnswerToACancelledAttempt => {
            tracing::debug!(
                "[{node_run_id}] its helper process answered command {command_id} for link \
                 '{link_id}', an attempt a newer one or the link's unwiring cancelled; dropped"
            );
        }
        OutOfProcessLinkWireAnswerNoted::DroppedAsTheAnswerToNoCommandAwaited => {
            tracing::warn!(
                "[{node_run_id}] its helper process answered command {command_id} for link \
                 '{link_id}', which no link is waiting on; dropped"
            );
        }
        OutOfProcessLinkWireAnswerNoted::RefusedItsAttemptForNamingAnotherLink {
            link_id_the_command_wired,
        } => {
            tracing::warn!(
                "[{node_run_id}] its helper process answered command {command_id}, which wired \
                 link '{link_id_the_command_wired}', for link '{link_id}'; that attempt is \
                 refused"
            );
        }
    }
}

/// Refuse every link this subprocess was still to answer for, and say so once.
pub(super) fn refuse_every_link_this_subprocess_still_owed(
    links_awaiting_their_wire_reply: &LinksAwaitingTheirOutOfProcessWireReply,
    node_run_id: &str,
    how_the_helper_was_noticed_gone: &str,
) {
    let refused =
        links_awaiting_their_wire_reply.refuse_every_link_still_awaiting_an_answer(&format!(
            "the helper process hosting '{node_run_id}' {how_the_helper_was_noticed_gone} before \
             it opened its port for this link"
        ));
    if refused > 0 {
        tracing::warn!(
            "[{}] its helper process {} with {} link(s) it never opened a port for; each reads \
             error rather than wired",
            node_run_id,
            how_the_helper_was_noticed_gone,
            refused
        );
    }
}

pub(super) fn node_run_id_shortened_for_a_thread_name(node_run_id: &str) -> &str {
    let end = node_run_id
        .char_indices()
        .nth(8)
        .map_or(node_run_id.len(), |(byte_index, _)| byte_index);
    &node_run_id[..end]
}
