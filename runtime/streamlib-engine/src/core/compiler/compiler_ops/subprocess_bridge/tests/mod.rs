// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use std::io::{BufReader, BufWriter, Write};
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

use crate::core::context::GpuContextLimitedAccess;
use crate::core::error::PortDirection;
use crate::core::nodes::{
    OutOfProcessFarSideLinkDelivery, OutOfProcessLinkWireAnswerNoted, OutOfProcessLinkWireOutcome,
    OutOfProcessLinkWireReply,
};

use crate::core::compiler::compiler_ops::subprocess_escalate::{
    ESCALATE_OP_ANSWERED_BY_NOTHING, ESCALATE_OP_BUILDING_A_KERNEL, WhatAHelperPlacedNodeReaches,
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
    fn a_kernel_build_is_queued_for_the_kernel_build_worker() {
        assert_eq!(
            classify_an_incoming_subprocess_frame(&escalate_request_awaiting_an_answer(
                ESCALATE_OP_BUILDING_A_KERNEL,
                "r-build"
            )),
            IncomingSubprocessFrame::EscalateRequestBuildingAKernel
        );
    }

    #[test]
    fn a_wire_answer_is_routed_to_its_link_rather_than_to_the_lifecycle_queue() {
        assert_eq!(
            classify_an_incoming_subprocess_frame(&serde_json::json!({
                "rpc": OUT_OF_PROCESS_LINK_WIRED_RPC,
                "command_id": 4,
                "link_id": "L-late",
            })),
            IncomingSubprocessFrame::LinkWireAnswer {
                command_id: OutOfProcessCommandId::from_the_wire(4),
                link_id: "L-late".to_string(),
                outcome: OutOfProcessLinkWireOutcome::OpenedByTheFarSide,
            },
            "routed to the lifecycle queue it would end the wait of the lifecycle command \
             the host is waiting on"
        );
    }

    #[test]
    fn a_refused_wire_carries_the_subprocesss_own_reason() {
        assert_eq!(
            classify_an_incoming_subprocess_frame(&serde_json::json!({
                "rpc": OUT_OF_PROCESS_LINK_WIRE_FAILED_RPC,
                "command_id": 5,
                "link_id": "L-late",
                "reason": "ExceedsMaxSupportedSubscribers",
            })),
            IncomingSubprocessFrame::LinkWireAnswer {
                command_id: OutOfProcessCommandId::from_the_wire(5),
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
            "command_id": 6,
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
                "command_id": 7,
            })),
            IncomingSubprocessFrame::LinkWireAnswerNamingNoLink
        );
    }

    #[test]
    fn a_wire_answer_naming_no_command_is_never_read_as_a_lifecycle_reply() {
        assert_eq!(
            classify_an_incoming_subprocess_frame(&serde_json::json!({
                "rpc": OUT_OF_PROCESS_LINK_WIRED_RPC,
                "link_id": "L-late",
            })),
            IncomingSubprocessFrame::LinkWireAnswerNamingNoCommand
        );
    }
}

mod lifecycle_commands_to_one_subprocess;
mod reader_and_escalate_workers;

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

    fn command_id_of_the_frame(frame: &serde_json::Value) -> OutOfProcessCommandId {
        out_of_process_command_id_of(frame).expect("a command expecting an answer carries its id")
    }

    /// The race at the seam the compiler drives: a link handed over, unwired
    /// and handed over again before the subprocess's first answer is read.
    /// The subprocess answers in order, the first attempt opened and the
    /// second refused.
    ///
    /// Fail-without-fix: match the answer by link id alone and the first
    /// answer settles the second attempt's cell, so the link reads `wired`
    /// though its new wiring failed.
    #[test]
    fn a_link_wired_again_before_its_first_answer_reads_as_the_second_attempt_answered() {
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
        let first_attempt =
            command_id_of_the_frame(&read_frame(&mut subprocess_end_reader).expect("wire_link"));
        delivery
            .tell_the_far_side_a_link_was_unwired(
                PortDirection::Input,
                "frames_from_upstream",
                "L-late",
            )
            .expect("a live subprocess is told");
        read_frame(&mut subprocess_end_reader).expect("the unwire_link frame");
        let second_attempts_cell = OutOfProcessLinkWireReply::awaiting_the_far_sides_answer();
        delivery
            .hand_over_a_link_wired_after_setup(
                PortDirection::Input,
                &late_input_link(),
                Arc::clone(&second_attempts_cell),
            )
            .expect("a live subprocess takes the link again");
        let second_attempt =
            command_id_of_the_frame(&read_frame(&mut subprocess_end_reader).expect("wire_link"));
        assert_ne!(
            first_attempt, second_attempt,
            "each attempt goes out with an id of its own"
        );

        let awaiting = &delivery.parent_side.links_awaiting_their_wire_reply;
        assert_eq!(
            awaiting.note_the_far_sides_answer_to(
                first_attempt,
                "L-late",
                OutOfProcessLinkWireOutcome::OpenedByTheFarSide
            ),
            OutOfProcessLinkWireAnswerNoted::DroppedAsTheAnswerToACancelledAttempt
        );
        assert_eq!(
            awaiting.note_the_far_sides_answer_to(
                second_attempt,
                "L-late",
                OutOfProcessLinkWireOutcome::RefusedByTheFarSide {
                    reason: "no such service".to_string()
                }
            ),
            OutOfProcessLinkWireAnswerNoted::SettledItsAttempt
        );
        assert_eq!(
            second_attempts_cell.the_far_sides_answer(),
            Some(OutOfProcessLinkWireOutcome::RefusedByTheFarSide {
                reason: "no such service".to_string()
            }),
            "the link reads its second attempt's own answer, never the first attempt's"
        );
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

        let wire_link =
            read_frame(&mut subprocess_end_reader).expect("the frame reached the subprocess");
        let command_id = command_id_of_the_frame(&wire_link);
        assert_eq!(
            wire_link,
            serde_json::json!({
                "cmd": "wire_link",
                "command_id": command_id,
                "direction": "input",
                "link": late_input_link(),
            })
        );
        assert_eq!(
            delivery
                .parent_side
                .links_awaiting_their_wire_reply
                .note_the_far_sides_answer_to(
                    command_id,
                    "L-late",
                    OutOfProcessLinkWireOutcome::OpenedByTheFarSide
                ),
            OutOfProcessLinkWireAnswerNoted::SettledItsAttempt,
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
        let wire_link = command_id_of_the_frame(
            &read_frame(&mut subprocess_end_reader).expect("the wire_link frame"),
        );

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
        assert_eq!(
            delivery
                .parent_side
                .links_awaiting_their_wire_reply
                .note_the_far_sides_answer_to(
                    wire_link,
                    "L-late",
                    OutOfProcessLinkWireOutcome::OpenedByTheFarSide
                ),
            OutOfProcessLinkWireAnswerNoted::DroppedAsTheAnswerToACancelledAttempt,
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
    let setup_sent = bridge
        .send_a_lifecycle_command_awaiting_its_reply(
            LifecycleCommandAnsweredByAReply::Setup,
            LifecycleCommandFields::new(),
        )
        .expect("the setup command goes out");
    match bridge.await_the_reply_to(setup_sent, Duration::from_millis(250)) {
        Err(LifecycleReplyNotReceived::DeadlinePassed { .. }) => {}
        Ok(frame) => panic!(
            "log frame leaked to lifecycle channel \
             — reader_loop classification has regressed: {frame}"
        ),
        Err(other) => panic!(
            "the log frame reached the lifecycle queue, or the bridge reader exited before the \
             test could assert: {other}"
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

    let setup_sent = bridge
        .send_a_lifecycle_command_awaiting_its_reply(
            LifecycleCommandAnsweredByAReply::Setup,
            LifecycleCommandFields::new(),
        )
        .expect("the setup command goes out");
    let ready = serde_json::json!({"rpc": "ready", "command_id": setup_sent.command_id});
    let mut child_writer = BufWriter::new(child_end);
    write_frame(&mut child_writer, &ready).expect("write ready frame");
    drop(child_writer);

    let got = bridge
        .await_the_reply_to(setup_sent, Duration::from_millis(500))
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
