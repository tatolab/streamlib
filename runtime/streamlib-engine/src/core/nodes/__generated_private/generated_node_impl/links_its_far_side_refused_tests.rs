// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use crate::core::PortDirection;
use crate::core::test_support::RecordingOutOfProcessFarSideLinkDelivery;

fn link_wiring_entry(link_id: &str, port_name: &str) -> JsonValue {
    serde_json::json!({ "name": port_name, "link_id": link_id })
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
