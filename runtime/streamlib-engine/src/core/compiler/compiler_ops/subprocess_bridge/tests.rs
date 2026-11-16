// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use std::io::{BufReader, BufWriter, Write};
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::Duration;

use crate::core::context::GpuContextLimitedAccess;
use crate::core::error::PortDirection;
use crate::core::nodes::{
    OutOfProcessFarSideLinkDelivery, OutOfProcessLinkWireOutcome, OutOfProcessLinkWireReply,
};

use crate::core::compiler::compiler_ops::subprocess_escalate::{
    ESCALATE_OP_ANSWERED_BY_NOTHING, WhatAHelperPlacedNodeReaches,
};

use crate::core::context::GpuContext;

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
                .expect("a log record sent behind a held escalate is dispatched while it is held"),
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
    use crate::core::nodes::OutOfProcessLinkWiringEnvelope;
    use crate::core::test_support::CapturedTracingWarnings;

    struct LinkDeliveryWithTheSubprocessEndInHand {
        delivery: SubprocessBridgeLinkDelivery,
        subprocess_end_reader: BufReader<UnixStream>,
    }

    fn link_delivery_with_the_subprocess_end_in_hand() -> LinkDeliveryWithTheSubprocessEndInHand {
        let (parent_end, subprocess_end) = UnixStream::pair().expect("socketpair");
        subprocess_end
            .set_read_timeout(Some(Duration::from_millis(200)))
            .expect("a read timeout on the subprocess end");
        LinkDeliveryWithTheSubprocessEndInHand {
            delivery: SubprocessBridgeLinkDelivery {
                node_display_name: "BlurNode".to_string(),
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
                .contains("'BlurNode' (Pblur) has failed"),
            "the refusal names the node; got {refused}"
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
        let envelope =
            OutOfProcessLinkWiringEnvelope::for_a_far_side_driven_in(ProcessExecution::Reactive);
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
            refusal_of_a_link_into_a_helper_process_that_failed("BlurNode", "Pblur"),
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
        "node_run_id": "p-bridge-test",
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
        None,
        sandbox,
        "p-bridge-test".into(),
        WhatAHelperPlacedNodeReaches::default(),
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
        None,
        sandbox,
        "p-bridge-test".into(),
        WhatAHelperPlacedNodeReaches::default(),
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

/// The native socket answers an escalate on its own, against the same
/// registry the bridge's worker uses, and answers anything else with a
/// refusal rather than leaving its caller waiting.
#[test]
fn the_native_escalate_socket_answers_escalates_and_refuses_everything_else() {
    const TEST: &str = "the_native_escalate_socket_answers_escalates_and_refuses_everything_else";
    let Some(sandbox) = gpu_sandbox_or_skip(TEST) else {
        return;
    };

    let (parent_end, _child_end) = UnixStream::pair().expect("socketpair");
    let (native_parent_end, native_child_end) = UnixStream::pair().expect("socketpair");
    let _bridge = SubprocessBridge::new(
        parent_end,
        Some(native_parent_end),
        sandbox,
        "p-native-escalate-test".into(),
        WhatAHelperPlacedNodeReaches::default(),
    )
    .expect("bridge construction");

    let mut native_writer = BufWriter::new(native_child_end.try_clone().expect("clone native end"));
    let mut native_reader = BufReader::new(native_child_end);
    write_frame(
        &mut native_writer,
        &serde_json::json!({
            "rpc": "escalate_request",
            "op": "acquire_pixel_buffer",
            "request_id": "r-native-acquire",
            "width": 16,
            "height": 16,
            "format": "bgra32",
        }),
    )
    .expect("write acquire frame");
    let acquired = read_frame(&mut native_reader).expect("acquire answer");
    assert_eq!(acquired["request_id"], "r-native-acquire", "{acquired}");
    assert_eq!(acquired["result"], "ok", "{acquired}");
    let handle_id = acquired["handle_id"]
        .as_str()
        .expect("a handle_id")
        .to_string();

    write_frame(
        &mut native_writer,
        &serde_json::json!({
            "rpc": "escalate_request",
            "op": "release_handle",
            "request_id": "r-native-release",
            "handle_id": handle_id,
        }),
    )
    .expect("write release frame");
    let released = read_frame(&mut native_reader).expect("release answer");
    assert_eq!(released["result"], "ok", "{released}");

    write_frame(
        &mut native_writer,
        &serde_json::json!({"rpc": "ready", "request_id": "r-not-an-escalate"}),
    )
    .expect("write a lifecycle frame");
    let refused = read_frame(&mut native_reader).expect("refusal");
    assert_eq!(refused["request_id"], "r-not-an-escalate", "{refused}");
    assert_eq!(refused["result"], "err", "{refused}");
}

#[test]
fn the_compiled_engine_build_id_leads_with_this_crates_version() {
    assert!(
        ENGINE_BUILD_ID.starts_with(concat!(env!("CARGO_PKG_VERSION"), "+")),
        "{ENGINE_BUILD_ID}"
    );
}

/// The crash-path mirror of the explicit `release_handle` op: a
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
    let socket_dir =
        crate::core::test_support::a_temporary_directory_at_owner_only_mode().expect("socket dir");
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
    gpu.install_the_engines_surface_store(store)
        .expect("the engine's surface store installs once");
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
            None,
            sandbox.clone(),
            format!("p-crash-{cycle}"),
            WhatAHelperPlacedNodeReaches::default(),
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
