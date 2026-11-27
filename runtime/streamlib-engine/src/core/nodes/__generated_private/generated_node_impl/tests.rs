// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use crate::core::PortDirection;
use crate::core::test_support::{ReclaimedLink, RecordingOutOfProcessFarSideLinkDelivery};

fn link_wiring_entry(link_id: &str, port_name: &str) -> JsonValue {
    serde_json::json!({ "name": port_name, "link_id": link_id })
}

fn link_ids_the_setup_command_carries(ports: &JsonValue, direction: &str) -> Vec<String> {
    ports[direction]
        .as_array()
        .expect("the envelope renders both directions as arrays")
        .iter()
        .map(|link| link["link_id"].as_str().unwrap().to_string())
        .collect()
}

fn record(envelope: &OutOfProcessLinkWiringEnvelope, direction: PortDirection, link_id: &str) {
    envelope
        .record_a_link_and_hand_it_to_a_far_side_past_its_setup_command(
            direction,
            link_wiring_entry(link_id, "port"),
        )
        .expect("a far side that is not gone takes the link");
}

fn an_envelope_whose_island_calls_a_stateful_lfo() -> OutOfProcessLinkWiringEnvelope {
    let envelope =
        OutOfProcessLinkWiringEnvelope::for_a_far_side_driven_in(ProcessExecution::Reactive);
    envelope
        .record_a_function_node_island_and_hand_it_to_a_far_side_past_its_setup_command(
            serde_json::json!({
                "link_id": "L-island",
                "nodes": [
                    {"node": "P-remap", "name": "remap"},
                    {
                        "node": "P-lfo",
                        "name": "lfo",
                        "node_uuid": the_lfo_uuid(),
                        "state_fields": ["phase"],
                    },
                ],
            }),
        )
        .expect("a far side not yet set up takes its islands");
    envelope
}

fn the_lfo_uuid() -> uuid::Uuid {
    uuid::Uuid::from_u128(0x1f0)
}

fn the_lfo_at(phase: f64) -> crate::core::runtime::FunctionNodeState {
    crate::core::runtime::FunctionNodeState {
        state_fields: vec!["phase".to_string()],
        state_values: vec![phase],
    }
}

#[test]
fn an_island_recorded_again_notes_each_stateful_node_as_the_live_edit_left_it() {
    let envelope = an_envelope_whose_island_calls_a_stateful_lfo();
    envelope
        .record_a_function_node_island_and_hand_it_to_a_far_side_past_its_setup_command(
            serde_json::json!({
                "link_id": "L-island",
                "nodes": [
                    {"node": "P-remap", "name": "remap"},
                    {
                        "node": "P-lfo",
                        "name": "lfo",
                        "node_uuid": the_lfo_uuid(),
                        "state_fields": ["phase", "depth"],
                    },
                ],
            }),
        )
        .expect("a far side not yet set up takes its islands");

    let recorded = envelope
        .recorded_links_and_how_the_next_reaches_the_far_side
        .lock();
    let [lfo] = recorded
        .stateful_function_nodes_its_far_side_calls
        .as_slice()
    else {
        panic!("the lfo is noted once");
    };
    assert_eq!(lfo.function_node_uuid, the_lfo_uuid());
    assert_eq!(
        lfo.state_fields,
        vec!["phase".to_string(), "depth".to_string()]
    );
}

fn the_lfo_island_recorded_again_with(lfo: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "link_id": "L-island",
        "nodes": [{"node": "P-remap", "name": "remap"}, lfo],
    })
}

#[test]
fn a_node_a_live_edit_makes_stateless_is_no_longer_noted_stateful() {
    let envelope = an_envelope_whose_island_calls_a_stateful_lfo();
    envelope
        .record_a_function_node_island_and_hand_it_to_a_far_side_past_its_setup_command(
            the_lfo_island_recorded_again_with(serde_json::json!({
                "node": "P-lfo",
                "name": "lfo",
            })),
        )
        .expect("a far side not yet set up takes its islands");

    assert!(
        envelope
            .recorded_links_and_how_the_next_reaches_the_far_side
            .lock()
            .stateful_function_nodes_its_far_side_calls
            .is_empty()
    );
}

/// Each spawn's board carries an entry for the stateful node alone, and a
/// spawn after one that wrote a state resumes the node from it.
#[test]
fn a_new_spawn_resumes_each_stateful_node_from_the_state_the_last_spawn_wrote() {
    let envelope = an_envelope_whose_island_calls_a_stateful_lfo();
    let function_node_states = Arc::new(FunctionNodeStatesCarriedForOneStream::default());
    let parent_node = Iceoryx2Node::for_this_test_process();

    let first_spawns_board = envelope
        .create_the_loss_count_board_for_this_helper_spawn_on(
            &parent_node,
            &function_node_states,
            "P-reader",
            Vec::new(),
        )
        .expect("the first spawn creates its board");
    assert_eq!(
        first_spawns_board["function_node_states"],
        serde_json::json!([{"breadcrumb_number": 1, "resumes_from_f64_bits": null}]),
        "only the stateful node has an entry, and nothing is carried for it yet"
    );
    let first_helper_node = Iceoryx2Node::for_this_test_process();
    let first_helpers_writer = first_helper_node
        .open_helper_process_loss_count_board_writer(
            first_spawns_board["service_name"].as_str().unwrap(),
            &[],
            &[1],
        )
        .expect("the helper opens its board");
    first_helpers_writer
        .write_the_state_of_a_function_node(1, &[0.25])
        .unwrap();
    assert_eq!(
        function_node_states.the_current_state_of(the_lfo_uuid()),
        Some(the_lfo_at(0.25)),
        "the stream reads the state live off the board its helper writes"
    );
    drop(first_helpers_writer);
    envelope.keep_every_function_node_state_its_far_side_wrote();

    let second_spawns_board = envelope
        .create_the_loss_count_board_for_this_helper_spawn_on(
            &parent_node,
            &function_node_states,
            "P-reader",
            Vec::new(),
        )
        .expect("the restart creates its own board");
    assert_eq!(
        second_spawns_board["function_node_states"],
        serde_json::json!([{
            "breadcrumb_number": 1,
            "resumes_from_f64_bits": [0.25f64.to_bits()],
        }])
    );
    assert_eq!(
        function_node_states.the_current_state_of(the_lfo_uuid()),
        Some(the_lfo_at(0.25)),
        "the state the first spawn left stands until the second writes"
    );
}

/// A promoted node's own helper takes over writing its state, and the
/// reader's restart afterwards leaves the state the promoted node writes.
#[test]
fn a_promoted_nodes_own_helper_takes_its_state_over_from_the_reader_it_crashed() {
    let reader = an_envelope_whose_island_calls_a_stateful_lfo();
    let function_node_states = Arc::new(FunctionNodeStatesCarriedForOneStream::default());
    let parent_node = Iceoryx2Node::for_this_test_process();
    let readers_board = reader
        .create_the_loss_count_board_for_this_helper_spawn_on(
            &parent_node,
            &function_node_states,
            "P-reader",
            Vec::new(),
        )
        .unwrap();
    let readers_helper_node = Iceoryx2Node::for_this_test_process();
    readers_helper_node
        .open_helper_process_loss_count_board_writer(
            readers_board["service_name"].as_str().unwrap(),
            &[],
            &[1],
        )
        .unwrap()
        .write_the_state_of_a_function_node(1, &[0.5])
        .unwrap();

    let promoted =
        OutOfProcessLinkWiringEnvelope::for_a_far_side_driven_in(ProcessExecution::Reactive);
    promoted.number_the_function_node_its_far_side_runs_as_a_standalone_node(
        "P-lfo",
        "lfo",
        the_lfo_uuid(),
        vec!["phase".to_string()],
    );
    let promoted_board = promoted
        .create_the_loss_count_board_for_this_helper_spawn_on(
            &parent_node,
            &function_node_states,
            "P-lfo",
            Vec::new(),
        )
        .unwrap();
    assert_eq!(
        promoted_board["function_node_states"][0]["resumes_from_f64_bits"],
        serde_json::json!([0.5f64.to_bits()]),
        "the promoted node resumes from what its reader's helper last wrote"
    );
    let promoted_helper_node = Iceoryx2Node::for_this_test_process();
    promoted_helper_node
        .open_helper_process_loss_count_board_writer(
            promoted_board["service_name"].as_str().unwrap(),
            &[],
            &[0],
        )
        .unwrap()
        .write_the_state_of_a_function_node(0, &[0.75])
        .unwrap();

    reader.keep_every_function_node_state_its_far_side_wrote();
    assert_eq!(
        function_node_states.the_current_state_of(the_lfo_uuid()),
        Some(the_lfo_at(0.75)),
        "the reader letting its helper go never takes the state back"
    );
}

/// A reader whose island moved out with its stateful node carries no
/// entry for that node on its next spawn's board, so the node's own helper
/// stays the one whose state the stream carries.
#[test]
fn a_readers_spawn_after_its_island_moved_out_never_takes_the_moved_nodes_state_over() {
    let reader = an_envelope_whose_island_calls_a_stateful_lfo();
    let function_node_states = Arc::new(FunctionNodeStatesCarriedForOneStream::default());
    let parent_node = Iceoryx2Node::for_this_test_process();
    reader.forget_a_function_node_island("L-island");
    let promoted =
        OutOfProcessLinkWiringEnvelope::for_a_far_side_driven_in(ProcessExecution::Reactive);
    promoted.number_the_function_node_its_far_side_runs_as_a_standalone_node(
        "P-lfo",
        "lfo",
        the_lfo_uuid(),
        vec!["phase".to_string()],
    );
    promoted.forget_a_function_node_island("L-an-island-feeding-the-promoted-node");
    let promoted_board = promoted
        .create_the_loss_count_board_for_this_helper_spawn_on(
            &parent_node,
            &function_node_states,
            "P-lfo",
            Vec::new(),
        )
        .unwrap();
    let promoted_helper_node = Iceoryx2Node::for_this_test_process();
    promoted_helper_node
        .open_helper_process_loss_count_board_writer(
            promoted_board["service_name"].as_str().unwrap(),
            &[],
            &[0],
        )
        .unwrap()
        .write_the_state_of_a_function_node(0, &[0.25])
        .unwrap();

    let readers_board = reader
        .create_the_loss_count_board_for_this_helper_spawn_on(
            &parent_node,
            &function_node_states,
            "P-reader",
            Vec::new(),
        )
        .unwrap();

    assert_eq!(
        readers_board["function_node_states"],
        serde_json::json!([]),
        "the reader evaluates the moved node no more"
    );
    assert_eq!(
        function_node_states.the_current_state_of(the_lfo_uuid()),
        Some(the_lfo_at(0.25)),
        "the moved node's own helper stays the one whose state the stream carries"
    );
}

/// A host that stopped waiting leaves no promotion for a late compile to
/// claim, so its restarted far side keeps the islands it started with.
#[test]
fn a_promotion_the_host_stopped_waiting_on_is_not_claimed_by_a_late_compile() {
    let envelope =
        OutOfProcessLinkWiringEnvelope::for_a_far_side_driven_in(ProcessExecution::Reactive);
    let given_up = envelope.await_a_function_node_promotion_out_of_its_islands();

    let outcome = envelope.wait_for_the_function_node_promotion_out_of_its_islands_to_settle(
        std::time::Duration::ZERO,
        || false,
    );

    assert_eq!(outcome, None);
    assert!(!envelope.claim_the_awaited_function_node_promotion_out_of_its_islands(given_up));
}

/// A request the host gave up cannot claim the one it awaits after the next
/// death: only the compile carrying the awaited request plans a promotion.
#[test]
fn an_earlier_promotion_request_cannot_claim_the_one_the_host_now_awaits() {
    let envelope =
        OutOfProcessLinkWiringEnvelope::for_a_far_side_driven_in(ProcessExecution::Reactive);
    let given_up = envelope.await_a_function_node_promotion_out_of_its_islands();
    envelope.give_up_the_function_node_promotion_out_of_its_islands_unless_claimed();
    let awaited_now = envelope.await_a_function_node_promotion_out_of_its_islands();

    assert!(!envelope.claim_the_awaited_function_node_promotion_out_of_its_islands(given_up));
    assert!(envelope.claim_the_awaited_function_node_promotion_out_of_its_islands(awaited_now));
}

/// Once a compile claims the promotion, the host waits past its deadline
/// for how it ends.
#[test]
fn a_claimed_promotion_is_waited_on_past_the_deadline_until_it_settles() {
    let envelope = Arc::new(OutOfProcessLinkWiringEnvelope::for_a_far_side_driven_in(
        ProcessExecution::Reactive,
    ));
    let awaited = envelope.await_a_function_node_promotion_out_of_its_islands();
    assert!(envelope.claim_the_awaited_function_node_promotion_out_of_its_islands(awaited));
    assert!(
        !envelope.give_up_the_function_node_promotion_out_of_its_islands_unless_claimed(),
        "a claimed promotion is not given up"
    );
    let settling_envelope = Arc::clone(&envelope);
    let settler = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(150));
        settling_envelope
            .settle_the_function_node_promotion_out_of_its_islands(Err("refused".to_string()));
    });

    let outcome = envelope.wait_for_the_function_node_promotion_out_of_its_islands_to_settle(
        std::time::Duration::ZERO,
        || false,
    );

    settler.join().unwrap();
    assert_eq!(outcome, Some(Err("refused".to_string())));
}

/// A disconnected link leaves the envelope in both directions, and only
/// that link does. Leave it behind and the next setup re-sends it beside
/// the reconnect's own entry — two subscribers, two notifiers, one link.
#[test]
fn a_removed_link_leaves_the_envelope_and_its_neighbours_stay() {
    let envelope =
        OutOfProcessLinkWiringEnvelope::for_a_far_side_driven_in(ProcessExecution::Reactive);
    record(&envelope, PortDirection::Input, "L-gone");
    record(&envelope, PortDirection::Input, "L-stays");
    record(&envelope, PortDirection::Output, "L-gone");
    record(&envelope, PortDirection::Output, "L-stays");

    envelope
        .forget_a_link_and_tell_a_far_side_past_its_setup_command(
            PortDirection::Input,
            "port",
            "L-gone",
        )
        .expect("a far side that was never set up needs no telling");

    let ports = envelope.as_setup_command_ports();
    assert_eq!(
        link_ids_the_setup_command_carries(&ports, "inputs"),
        ["L-stays"]
    );
    assert_eq!(
        link_ids_the_setup_command_carries(&ports, "outputs"),
        ["L-stays"]
    );
}

/// Removing a link the envelope never carried changes nothing — the
/// compiler reclaims every link it closes, including ones whose far side
/// was never wired.
#[test]
fn removing_an_unknown_link_leaves_the_envelope_alone() {
    let envelope =
        OutOfProcessLinkWiringEnvelope::for_a_far_side_driven_in(ProcessExecution::Reactive);
    record(&envelope, PortDirection::Output, "L-only");

    envelope
        .forget_a_link_and_tell_a_far_side_past_its_setup_command(
            PortDirection::Output,
            "port",
            "L-never-recorded",
        )
        .expect("a far side that was never set up needs no telling");

    assert_eq!(
        link_ids_the_setup_command_carries(&envelope.as_setup_command_ports(), "outputs"),
        ["L-only"]
    );
}

/// A link recorded before the setup command goes out rides it and waits on
/// no answer of its own; one recorded after is handed to the far side and
/// answered for.
///
/// Fail-without-fix: leave the envelope riding the setup command once it
/// has gone out and the later link is recorded where nothing reads it — a
/// link `graph` reports wired that no port was ever opened for.
#[test]
fn a_link_recorded_before_the_setup_command_rides_it_and_one_recorded_after_is_handed_over() {
    let envelope =
        OutOfProcessLinkWiringEnvelope::for_a_far_side_driven_in(ProcessExecution::Reactive);
    let before = envelope
        .record_a_link_and_hand_it_to_a_far_side_past_its_setup_command(
            PortDirection::Input,
            link_wiring_entry("L-before", "in1"),
        )
        .expect("a far side not yet set up takes the link");
    assert!(before.is_none());

    let far_side = RecordingOutOfProcessFarSideLinkDelivery::default();
    let mut setup_command_ports = None;
    envelope
        .send_the_setup_command_then_hand_every_later_link_over(
            |ports| {
                setup_command_ports = Some(ports);
                Ok(())
            },
            far_side.clone(),
        )
        .expect("the setup command goes out");
    assert_eq!(
        link_ids_the_setup_command_carries(&setup_command_ports.unwrap(), "inputs"),
        ["L-before"]
    );

    let after = envelope
        .record_a_link_and_hand_it_to_a_far_side_past_its_setup_command(
            PortDirection::Input,
            link_wiring_entry("L-after", "in1"),
        )
        .expect("a far side past its setup command takes the link");
    assert!(
        after.is_some(),
        "a link handed over waits on its far side's answer"
    );
    let handed_over = far_side.late_wired_links.lock();
    let [(PortDirection::Input, entry)] = &handed_over[..] else {
        panic!("exactly the later link is handed over; got {handed_over:?}");
    };
    assert_eq!(entry["link_id"], "L-after");
}

/// A link recorded while the setup command is going out waits for it, then
/// is handed over behind it — never recorded into a snapshot already taken
/// and never handed to a far side that has not been set up.
///
/// Fail-without-fix: take the setup command's `ports` outside the lock and
/// the link recorded meanwhile is neither in them nor handed over.
#[test]
fn a_link_recorded_while_the_setup_command_goes_out_is_handed_over_behind_it() {
    let envelope = Arc::new(OutOfProcessLinkWiringEnvelope::for_a_far_side_driven_in(
        ProcessExecution::Reactive,
    ));
    let far_side = RecordingOutOfProcessFarSideLinkDelivery::default();
    let (setup_command_is_going_out_tx, setup_command_is_going_out_rx) = std::sync::mpsc::channel();
    let (let_the_setup_command_finish_tx, let_the_setup_command_finish_rx) =
        std::sync::mpsc::channel::<()>();

    std::thread::scope(|scope| {
        let setup_envelope = Arc::clone(&envelope);
        let setup_far_side = far_side.clone();
        let setup_thread = scope.spawn(move || {
            let mut setup_command_ports = None;
            setup_envelope
                .send_the_setup_command_then_hand_every_later_link_over(
                    |ports| {
                        setup_command_is_going_out_tx.send(()).unwrap();
                        let_the_setup_command_finish_rx.recv().unwrap();
                        setup_command_ports = Some(ports);
                        Ok(())
                    },
                    setup_far_side,
                )
                .expect("the setup command goes out");
            setup_command_ports.unwrap()
        });
        setup_command_is_going_out_rx.recv().unwrap();

        let (recorded_tx, recorded_rx) = std::sync::mpsc::channel();
        let recording_envelope = Arc::clone(&envelope);
        let recording_thread = scope.spawn(move || {
            let reply = recording_envelope
                .record_a_link_and_hand_it_to_a_far_side_past_its_setup_command(
                    PortDirection::Output,
                    link_wiring_entry("L-meanwhile", "out1"),
                )
                .expect("the link is taken");
            recorded_tx.send(()).unwrap();
            reply
        });
        assert!(
            recorded_rx
                .recv_timeout(std::time::Duration::from_millis(200))
                .is_err(),
            "a link recorded while the setup command goes out waits for it"
        );

        let_the_setup_command_finish_tx.send(()).unwrap();
        let setup_command_ports = setup_thread.join().unwrap();
        let reply = recording_thread.join().unwrap();

        assert!(
            link_ids_the_setup_command_carries(&setup_command_ports, "outputs").is_empty(),
            "the snapshot was taken before the link was recorded"
        );
        assert!(
            reply.is_some(),
            "so the link is handed over behind the command"
        );
        assert_eq!(far_side.late_wired_links.lock().len(), 1);
    });
}

/// Once the host gives its far side up, a link recorded after is refused
/// with the host's reason, and a disconnect tells nobody.
#[test]
fn a_link_recorded_once_the_far_side_is_gone_is_refused_naming_why() {
    let envelope =
        OutOfProcessLinkWiringEnvelope::for_a_far_side_driven_in(ProcessExecution::Reactive);
    let far_side = RecordingOutOfProcessFarSideLinkDelivery::default();
    envelope
        .send_the_setup_command_then_hand_every_later_link_over(|_| Ok(()), far_side.clone())
        .expect("the setup command goes out");

    envelope.refuse_every_later_link_because_the_far_side_is_gone(
        crate::core::compiler::compiler_ops::subprocess_bridge::refusal_of_a_link_into_a_helper_process_that_failed(
            "Blur", "Pblur",
        ),
    );

    let refused = envelope
        .record_a_link_and_hand_it_to_a_far_side_past_its_setup_command(
            PortDirection::Input,
            link_wiring_entry("L-too-late", "in1"),
        )
        .expect_err("a far side that is gone can open no port");
    assert!(refused.to_string().contains("has failed"), "{refused}");
    envelope
        .forget_a_link_and_tell_a_far_side_past_its_setup_command(
            PortDirection::Input,
            "in1",
            "L-too-late",
        )
        .expect("a far side that is gone needs no telling");
    assert!(far_side.late_wired_links.lock().is_empty());
    assert!(far_side.reclaimed_links.lock().is_empty());
}

fn refusal_reason_of(answer_cell: &OutOfProcessLinkWireReply) -> Option<String> {
    match answer_cell.the_far_sides_answer() {
        Some(OutOfProcessLinkWireOutcome::RefusedByTheFarSide { reason }) => Some(reason),
        _ => None,
    }
}

/// A link recorded once the far side's setup has begun is not folded into a
/// setup command whose `ports` may already be taken: it waits, reads as
/// awaiting its own answer, and is handed over right behind the command.
///
/// Fail-without-fix: let it ride the setup command and `connect` reports a
/// link `wired` that no helper has confirmed.
#[test]
fn a_link_recorded_once_setup_has_begun_is_handed_over_right_behind_the_setup_command() {
    let envelope =
        OutOfProcessLinkWiringEnvelope::for_a_far_side_driven_in(ProcessExecution::Reactive);
    record(&envelope, PortDirection::Input, "L-startup");
    envelope.hold_every_later_link_until_the_setup_command_goes_out();

    let answer_cell = envelope
        .record_a_link_and_hand_it_to_a_far_side_past_its_setup_command(
            PortDirection::Output,
            link_wiring_entry("L-live", "out1"),
        )
        .expect("a far side setting up takes the link")
        .expect("a link recorded during setup waits on its own answer");

    let far_side = RecordingOutOfProcessFarSideLinkDelivery::default();
    let mut setup_command_ports = None;
    envelope
        .send_the_setup_command_then_hand_every_later_link_over(
            |ports| {
                assert!(
                    far_side.late_wired_links.lock().is_empty(),
                    "nothing is handed over ahead of the setup command"
                );
                setup_command_ports = Some(ports);
                Ok(())
            },
            far_side.clone(),
        )
        .expect("the setup command goes out");

    let ports = setup_command_ports.expect("the setup command was sent");
    assert_eq!(
        link_ids_the_setup_command_carries(&ports, "inputs"),
        ["L-startup"]
    );
    assert!(link_ids_the_setup_command_carries(&ports, "outputs").is_empty());
    let handed_over = far_side.late_wired_links.lock();
    let [(PortDirection::Output, entry)] = &handed_over[..] else {
        panic!("exactly the live link is handed over; got {handed_over:?}");
    };
    assert_eq!(entry["link_id"], "L-live");
    assert!(Arc::ptr_eq(
        &far_side.wire_answers_owed.lock()[0],
        &answer_cell
    ));
}

/// A link waiting behind a setup command that could not be sent is refused
/// with the reason, and so is every link recorded after.
#[test]
fn a_setup_command_that_could_not_be_sent_refuses_the_links_waiting_behind_it() {
    let envelope =
        OutOfProcessLinkWiringEnvelope::for_a_far_side_driven_in(ProcessExecution::Reactive);
    envelope.hold_every_later_link_until_the_setup_command_goes_out();
    let answer_cell = envelope
        .record_a_link_and_hand_it_to_a_far_side_past_its_setup_command(
            PortDirection::Input,
            link_wiring_entry("L-waiting", "in1"),
        )
        .expect("a far side setting up takes the link")
        .expect("a link recorded during setup waits on its own answer");

    envelope
        .send_the_setup_command_then_hand_every_later_link_over(
            |_| {
                Err(crate::core::error::Error::Runtime(
                    "socket closed".to_string(),
                ))
            },
            RecordingOutOfProcessFarSideLinkDelivery::default(),
        )
        .expect_err("the send failed");

    let reason = refusal_reason_of(&answer_cell).expect("the waiting link is refused");
    assert!(reason.contains("socket closed"), "{reason}");
    envelope
        .record_a_link_and_hand_it_to_a_far_side_past_its_setup_command(
            PortDirection::Input,
            link_wiring_entry("L-after", "in1"),
        )
        .expect_err("a far side whose setup never went out takes no link");
}

/// Giving the far side up before its setup command goes out refuses the
/// links waiting behind it; a link disconnected while waiting is never
/// handed over.
#[test]
fn a_waiting_link_is_refused_when_the_far_side_is_given_up_and_dropped_when_disconnected() {
    let envelope =
        OutOfProcessLinkWiringEnvelope::for_a_far_side_driven_in(ProcessExecution::Reactive);
    envelope.hold_every_later_link_until_the_setup_command_goes_out();
    let disconnected = envelope
        .record_a_link_and_hand_it_to_a_far_side_past_its_setup_command(
            PortDirection::Input,
            link_wiring_entry("L-disconnected", "in1"),
        )
        .unwrap()
        .unwrap();
    envelope
        .forget_a_link_and_tell_a_far_side_past_its_setup_command(
            PortDirection::Input,
            "in1",
            "L-disconnected",
        )
        .expect("a waiting link needs no far side told");
    let given_up = envelope
        .record_a_link_and_hand_it_to_a_far_side_past_its_setup_command(
            PortDirection::Input,
            link_wiring_entry("L-given-up", "in1"),
        )
        .unwrap()
        .unwrap();

    envelope.refuse_every_later_link_because_the_far_side_is_gone(
        crate::core::compiler::compiler_ops::subprocess_bridge::refusal_of_a_link_into_a_helper_process_that_failed(
            "Blur", "Pblur",
        ),
    );

    assert!(refusal_reason_of(&given_up).is_some_and(|reason| reason.contains("has failed")));
    assert_eq!(
        disconnected.the_far_sides_answer(),
        None,
        "a link the graph no longer has is answered for by nobody"
    );
}

/// A disconnect asks a far side past its setup command to drop its port, by
/// its own port and direction.
#[test]
fn forgetting_a_link_tells_a_far_side_past_its_setup_command_which_port_to_drop() {
    let envelope =
        OutOfProcessLinkWiringEnvelope::for_a_far_side_driven_in(ProcessExecution::Reactive);
    let far_side = RecordingOutOfProcessFarSideLinkDelivery::default();
    envelope
        .send_the_setup_command_then_hand_every_later_link_over(|_| Ok(()), far_side.clone())
        .expect("the setup command goes out");
    record(&envelope, PortDirection::Output, "L-out");

    envelope
        .forget_a_link_and_tell_a_far_side_past_its_setup_command(
            PortDirection::Output,
            "out1",
            "L-out",
        )
        .expect("the far side is told");

    assert_eq!(
        *far_side.reclaimed_links.lock(),
        [ReclaimedLink {
            port_direction: PortDirection::Output,
            local_port_name: "out1".to_string(),
            link_id: "L-out".to_string(),
        }]
    );
    assert!(
        link_ids_the_setup_command_carries(&envelope.as_setup_command_ports(), "outputs")
            .is_empty()
    );
}

/// A new helper standing in for one that died is handed, on its own setup
/// command, every link the dead helper opened, those that rode its setup
/// command and those handed over past it alike, and never one it was handed
/// and never opened, which `graph` reads `error`.
///
/// Fail-without-fix: leave the envelope refusing and the new helper's setup
/// command is refused; clear the recorded links on reopen and it opens no
/// port; keep every recorded link and the unopened one rides along.
#[test]
fn a_reopened_envelope_hands_a_new_far_side_every_link_the_dead_one_held() {
    let envelope =
        OutOfProcessLinkWiringEnvelope::for_a_far_side_driven_in(ProcessExecution::Reactive);
    record(&envelope, PortDirection::Input, "L-before-setup");
    envelope.hold_every_later_link_until_the_setup_command_goes_out();
    envelope
        .send_the_setup_command_then_hand_every_later_link_over(
            |_| Ok(()),
            RecordingOutOfProcessFarSideLinkDelivery::default(),
        )
        .expect("the dead helper's setup command went out");
    envelope
        .record_a_link_and_hand_it_to_a_far_side_past_its_setup_command(
            PortDirection::Output,
            link_wiring_entry("L-past-setup", "port"),
        )
        .expect("a far side that is not gone takes the link")
        .expect("a link past the setup command waits on the far side's answer")
        .note_the_far_sides_answer(OutOfProcessLinkWireOutcome::OpenedByTheFarSide);
    record(&envelope, PortDirection::Output, "L-never-opened");
    envelope.refuse_every_later_link_because_the_far_side_is_gone(
        "the helper process died".to_string(),
    );

    envelope.reopen_for_a_new_far_side_in_place_of_one_that_died();
    envelope.hold_every_later_link_until_the_setup_command_goes_out();
    let mut new_far_sides_setup_command_ports = None;
    envelope
        .send_the_setup_command_then_hand_every_later_link_over(
            |ports| {
                new_far_sides_setup_command_ports = Some(ports);
                Ok(())
            },
            RecordingOutOfProcessFarSideLinkDelivery::default(),
        )
        .expect("the new helper's setup command goes out");

    let ports = new_far_sides_setup_command_ports.expect("the setup command carried ports");
    assert_eq!(
        link_ids_the_setup_command_carries(&ports, "inputs"),
        ["L-before-setup"]
    );
    assert_eq!(
        link_ids_the_setup_command_carries(&ports, "outputs"),
        ["L-past-setup"]
    );
}

fn an_island_of(input_port: &str, function_node_ids: &[&str]) -> JsonValue {
    serde_json::json!({
        "input_port": input_port,
        "link_id": format!("L-{input_port}"),
        "output": {"node": function_node_ids.last().unwrap(), "port": "result"},
        "nodes": function_node_ids
            .iter()
            .map(|node_id| serde_json::json!({"node": node_id, "links_into": []}))
            .collect::<Vec<_>>(),
    })
}

/// The far side's `ready` says how it ran each function node, and only the
/// nodes its islands call are told; a bool is all it reads.
#[test]
fn each_function_node_is_told_how_the_far_side_ran_it() {
    let envelope =
        OutOfProcessLinkWiringEnvelope::for_a_far_side_driven_in(ProcessExecution::Reactive);
    let how_lfo_ran = Arc::new(HowItsHelpersRanAFunctionNode::default());
    let how_tally_ran = Arc::new(HowItsHelpersRanAFunctionNode::default());
    envelope.tell_each_function_node_how_its_far_side_ran_it([
        ("lfo-1".to_string(), Arc::clone(&how_lfo_ran)),
        ("tally-1".to_string(), Arc::clone(&how_tally_ran)),
    ]);

    envelope.note_how_its_far_side_ran_its_function_nodes(Some(&serde_json::json!({
        "lfo-1": true,
        "tally-1": false,
        "never-carried": false,
    })));
    assert_eq!(how_lfo_ran.ran_compiled(), Some(true));
    assert_eq!(how_tally_ran.ran_compiled(), Some(false));

    envelope.note_how_its_far_side_ran_its_function_nodes(Some(&serde_json::json!({
        "lfo-1": "yes",
    })));
    envelope.note_how_its_far_side_ran_its_function_nodes(None);
    assert_eq!(how_lfo_ran.ran_compiled(), Some(true));
}

/// Each function node an island calls rides the setup command with one
/// breadcrumb number, the same in every island that calls it, so the
/// number its far side writes names one node.
#[test]
fn each_function_node_rides_the_setup_command_with_one_breadcrumb_number() {
    let envelope =
        OutOfProcessLinkWiringEnvelope::for_a_far_side_driven_in(ProcessExecution::Reactive);
    envelope
        .record_a_function_node_island_and_hand_it_to_a_far_side_past_its_setup_command(
            an_island_of("brightness", &["lfo-1", "remap-1"]),
        )
        .unwrap();
    envelope
        .record_a_function_node_island_and_hand_it_to_a_far_side_past_its_setup_command(
            an_island_of("hue", &["lfo-1", "clamp-1"]),
        )
        .unwrap();

    let islands = envelope.as_setup_command_ports()["islands"].clone();
    let breadcrumb_numbers: Vec<Vec<(String, u64)>> = islands
        .as_array()
        .unwrap()
        .iter()
        .map(|island| {
            island["nodes"]
                .as_array()
                .unwrap()
                .iter()
                .map(|node| {
                    (
                        node["node"].as_str().unwrap().to_string(),
                        node["breadcrumb_number"].as_u64().unwrap(),
                    )
                })
                .collect()
        })
        .collect();
    assert_eq!(
        breadcrumb_numbers,
        [
            vec![("lfo-1".to_string(), 0), ("remap-1".to_string(), 1)],
            vec![("lfo-1".to_string(), 0), ("clamp-1".to_string(), 2)],
        ]
    );
}

/// An envelope holds a link or a function node island its far side refused while it records
/// it, and none once it is forgotten to be wired again; a link opened or still awaited is no
/// refusal.
///
/// Mental-revert: reading every answered link as refused retries a link the far side opened;
/// recording no answer of an island never retries an island a running reader refused.
#[test]
fn an_envelope_holds_a_refused_link_or_island_until_it_forgets_it_and_no_other_counts() {
    let envelope =
        OutOfProcessLinkWiringEnvelope::for_a_far_side_driven_in(ProcessExecution::Reactive);
    envelope
        .send_the_setup_command_then_hand_every_later_link_over(
            |_| Ok(()),
            RecordingOutOfProcessFarSideLinkDelivery::default(),
        )
        .expect("the setup command goes out");
    let answer_of = |link_id: &str| {
        envelope
            .record_a_link_and_hand_it_to_a_far_side_past_its_setup_command(
                PortDirection::Input,
                link_wiring_entry(link_id, "in1"),
            )
            .expect("a far side that is not gone takes the link")
            .expect("a link past the setup command waits on the far side's answer")
    };
    let opened = answer_of("L-opened");
    let _awaited = answer_of("L-awaited");
    opened.note_the_far_sides_answer(OutOfProcessLinkWireOutcome::OpenedByTheFarSide);
    assert!(!envelope.holds_a_link_its_far_side_refused());

    answer_of("L-refused").note_the_far_sides_answer(
        OutOfProcessLinkWireOutcome::RefusedByTheFarSide {
            reason: "could not wire input port \"in1\"".to_string(),
        },
    );
    assert!(envelope.holds_a_link_its_far_side_refused());

    envelope
        .forget_a_link_and_tell_a_far_side_past_its_setup_command(
            PortDirection::Input,
            "in1",
            "L-refused",
        )
        .expect("the far side is told");
    assert!(!envelope.holds_a_link_its_far_side_refused());

    envelope
        .record_a_function_node_island_and_hand_it_to_a_far_side_past_its_setup_command(
            serde_json::json!({
                "link_id": "L-island",
                "input_port": "in1",
                "nodes": [{"node": "P-remap", "name": "remap"}],
            }),
        )
        .expect("a far side that is not gone takes the island")
        .expect("an island past the setup command waits on the far side's answer")
        .note_the_far_sides_answer(OutOfProcessLinkWireOutcome::RefusedByTheFarSide {
            reason: "could not evaluate the island".to_string(),
        });
    assert!(envelope.holds_a_link_its_far_side_refused());

    envelope.forget_a_function_node_island("L-island");
    assert!(!envelope.holds_a_link_its_far_side_refused());
}

/// A link recorded while the setup command waits to go out, whose handover after it fails, is
/// held as a refusal of the far side that runs on, so it is wired again.
///
/// Mental-revert: refusing it without keeping its answer leaves a far side that runs on holding
/// no refused link, so nothing retries it until the stream starts again.
#[test]
fn a_link_whose_handover_after_the_setup_command_fails_is_held_as_refused() {
    let envelope =
        OutOfProcessLinkWiringEnvelope::for_a_far_side_driven_in(ProcessExecution::Reactive);
    envelope.hold_every_later_link_until_the_setup_command_goes_out();
    let answer = envelope
        .record_a_link_and_hand_it_to_a_far_side_past_its_setup_command(
            PortDirection::Input,
            link_wiring_entry("L-behind-the-setup-command", "in1"),
        )
        .expect("a far side that is not gone takes the link")
        .expect("a link behind the setup command waits on the far side's answer");
    let far_side = RecordingOutOfProcessFarSideLinkDelivery::default();
    far_side
        .refuses_links
        .store(true, std::sync::atomic::Ordering::SeqCst);

    envelope
        .send_the_setup_command_then_hand_every_later_link_over(|_| Ok(()), far_side)
        .expect("the setup command goes out");

    assert!(answer.the_far_side_refused());
    assert!(envelope.holds_a_link_its_far_side_refused());
}
