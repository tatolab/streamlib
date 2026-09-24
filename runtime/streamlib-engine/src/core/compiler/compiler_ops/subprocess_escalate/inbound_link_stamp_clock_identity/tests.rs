// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use super::handle_inbound_link_stamp_clock_identity;
use crate::core::compiler::compiler_ops::subprocess_escalate_wire_types::EscalateResponse;
use crate::core::runtime::mesh::a_mesh_link_ingress_table_carrying_nothing;

// ------------------------------------------------------------------------
// Which machine a remote link's stamps are taken on, answered for a helper
// ------------------------------------------------------------------------

/// The address the arms below ask about, in the spelling a helper knows a
/// remote link by — a display name with a space in it, which is legal on
/// the mesh and is why the channel it rides is hashed instead.
const A_REMOTE_LINKS_NAME: &str = "bench-cam-a1b2/Camera Source/video";

/// The answer's machine, or `None` for one that named none.
fn the_machine_answered(response: &EscalateResponse) -> Option<String> {
    match response {
        EscalateResponse::Ok(answered) => answered.stamp_clock_identity.clone(),
        EscalateResponse::Err(refused) => {
            panic!("expected an answer, got a refusal: {}", refused.message)
        }
    }
}

/// The whole of what a helper cannot work out for itself: it hands over the
/// link's name and the app process reads the machine off the ingress cell
/// that name addresses.
#[test]
fn a_helpers_question_is_answered_with_the_machine_the_mesh_is_carrying_from() {
    let table = a_mesh_link_ingress_table_carrying_nothing();
    let address =
        crate::core::graph::MeshPortAddress::parse(A_REMOTE_LINKS_NAME).expect("a legal address");
    let machine_clock = table.machine_clock_carried_from(&address);

    assert_eq!(
        the_machine_answered(&handle_inbound_link_stamp_clock_identity(
            &table,
            "req-1".to_string(),
            A_REMOTE_LINKS_NAME,
        )),
        None,
        "nothing has crossed the link, so no machine has been named"
    );

    let another_machine =
        crate::core::runtime::mesh::MachineClockIdentity::of_the_machine_whose_boot_session_uuid_reads(
            "8b93a1c2-0000-4d5a-9a11-2c7f0d5e2f1c",
        );
    machine_clock.note_the_machine_a_bag_was_stamped_on(another_machine);

    assert_eq!(
        the_machine_answered(&handle_inbound_link_stamp_clock_identity(
            &table,
            "req-2".to_string(),
            A_REMOTE_LINKS_NAME,
        )),
        Some("8b93a1c2-0000-4d5a-9a11-2c7f0d5e2f1c".to_string()),
        "the answer is the machine the ingress learnt off the wire"
    );
}

/// An address this runtime carries nothing from names no machine, rather
/// than borrowing this one — the answer that would let two clocks be
/// compared.
#[test]
fn an_address_this_runtime_carries_nothing_from_names_no_machine() {
    assert_eq!(
        the_machine_answered(&handle_inbound_link_stamp_clock_identity(
            &a_mesh_link_ingress_table_carrying_nothing(),
            "req-3".to_string(),
            A_REMOTE_LINKS_NAME,
        )),
        None
    );
}

/// Only a link carrying from another runtime ever asks, and such a link is
/// named by its source port's mesh address. A name that is not one is a
/// helper asking about a link it could have answered itself, so it is
/// refused by name rather than answered with silence a caller would read
/// as "not yet".
#[test]
fn a_link_name_that_is_not_a_mesh_address_is_refused_naming_it() {
    // The empty name is asserted on separately: `contains("")` is always
    // true, so it would pass this loop's message check without saying
    // anything.
    for not_an_address in [
        "pcamera/video_out",
        "one/two/three/four",
        "no-slashes-at-all",
    ] {
        let response = handle_inbound_link_stamp_clock_identity(
            &a_mesh_link_ingress_table_carrying_nothing(),
            "req-4".to_string(),
            not_an_address,
        );
        let EscalateResponse::Err(refused) = response else {
            panic!("{not_an_address:?} must be refused rather than answered");
        };
        assert!(
            refused.message.contains(not_an_address),
            "the refusal must name what was asked about, and reads {:?}",
            refused.message
        );
        assert_eq!(refused.request_id, "req-4", "the refusal correlates");
    }

    let EscalateResponse::Err(refused) = handle_inbound_link_stamp_clock_identity(
        &a_mesh_link_ingress_table_carrying_nothing(),
        "req-5".to_string(),
        "",
    ) else {
        panic!("an empty link name must be refused rather than answered");
    };
    assert!(
        refused.message.contains("mesh address"),
        "the refusal must say what it wanted, and reads {:?}",
        refused.message
    );
}
