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
use crate::core::processors::LinksAwaitingTheirOutOfProcessWireReply;

use crate::core::compiler::compiler_ops::subprocess_escalate::refusal_of_an_escalate_request;

use super::{
    ESCALATE_REQUESTS_QUEUED_PER_HELPER, EscalateRequestDispatch, IncomingSubprocessFrame,
    ParentSideOfOneSubprocessBridgeSharedByItsThreads, classify_an_incoming_subprocess_frame,
    read_frame, write_frame,
};

/// The two threads one bridge runs.
pub(super) struct SubprocessBridgeThreads {
    pub(super) frame_demultiplexing_reader_thread: JoinHandle<()>,
    pub(super) escalate_worker_thread: JoinHandle<()>,
}

/// Spawn the escalate worker, then the reader that feeds it.
///
/// The reader owns the only sender of the worker's queue, so the worker leaves
/// once the reader has and every queued request is taken off.
pub(super) fn spawn_the_reader_and_the_escalate_worker(
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
        .spawn(
            carrying_this_threads_project_log_route_entry_and_reading_a_helper_pipe(move || {
                escalate_worker_loop(escalate_requests_rx, &worker_parent_side, &worker_dispatch);
            }),
        )
        .map_err(|spawn_failure| {
            Error::Runtime(format!(
                "[{processor_id}] could not start its bridge's escalate worker: {spawn_failure}"
            ))
        })?;

    let frame_demultiplexing_reader_thread = thread::Builder::new()
        .name(bridge_reader_thread_name(&processor_id))
        .spawn(
            carrying_this_threads_project_log_route_entry_and_reading_a_helper_pipe(move || {
                reader_loop(
                    reader,
                    &parent_side,
                    &escalate_request_dispatch,
                    escalate_requests_tx,
                    lifecycle_tx,
                );
            }),
        )
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

/// Start the worker that answers the helper's native code over its own socket.
pub(super) fn spawn_the_native_escalate_worker(
    native_escalate_stream: UnixStream,
    parent_side: Arc<ParentSideOfOneSubprocessBridgeSharedByItsThreads>,
    escalate_request_dispatch: EscalateRequestDispatch,
) -> Result<JoinHandle<()>> {
    let processor_id = parent_side.processor_id.clone();
    thread::Builder::new()
        .name(bridge_native_escalate_worker_thread_name(&processor_id))
        .spawn(
            carrying_this_threads_project_log_route_entry_and_reading_a_helper_pipe(move || {
                native_escalate_worker_loop(
                    native_escalate_stream,
                    &parent_side,
                    &escalate_request_dispatch,
                );
            }),
        )
        .map_err(|spawn_failure| {
            Error::Runtime(format!(
                "[{processor_id}] could not start its native escalate worker: {spawn_failure}"
            ))
        })
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
    let processor_id = &parent_side.processor_id;
    let read_half = match native_escalate_stream.try_clone() {
        Ok(read_half) => read_half,
        Err(clone_failure) => {
            tracing::warn!(
                "[{processor_id}] native escalate worker could not clone its socket: \
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
                tracing::debug!("[{processor_id}] native escalate worker exiting: {read_failure}");
                return;
            }
        };
        if parent_side.the_subprocess_was_given_up_on() {
            return;
        }
        let answer = match classify_an_incoming_subprocess_frame(&request) {
            IncomingSubprocessFrame::EscalateRequestAwaitingAnAnswer => {
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
                "[{processor_id}] native escalate worker failed to write an answer: \
                 {write_failure}"
            );
            return;
        }
    }
}

/// Refuse every link this subprocess was still to answer for, and say so once.
pub(super) fn refuse_every_link_this_subprocess_still_owed(
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

/// At most the first eight characters of a processor id, cut on a character
/// boundary: Linux thread names are limited to 15 bytes.
pub(super) fn processor_id_shortened_for_a_thread_name(processor_id: &str) -> &str {
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

fn bridge_native_escalate_worker_thread_name(processor_id: &str) -> String {
    format!(
        "br-nat-{}",
        processor_id_shortened_for_a_thread_name(processor_id)
    )
}
