// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A link that starts at one member of a bundle hands its reader that member's own message,
//! taken from each message of the whole bundle as it arrives.

use serde_json::json;

use super::*;
use crate::core::descriptors::{BundleMember, PinType};
use crate::core::pin_value_packing::{PackedPinValueRef, pack_pin_value, unpack_pin_value};

fn a_reading() -> PinType {
    let member = |name: &str, member_type: PinType, default_value| BundleMember {
        name: name.to_owned(),
        member_type,
        meaning: None,
        default_value,
    };
    PinType::Bundle {
        members: vec![
            member("level", PinType::Float { exact_size: None }, None),
            member("label", PinType::String {}, Some(json!("unnamed"))),
        ],
    }
}

/// A port fed by one link that takes `member` of a reading at the read, and its publisher.
fn a_port_reading_a_member(
    mailboxes: &InputMailboxesInner,
    member: &str,
) -> TestChannelPublisherNumberingItsSends {
    let (publisher, subscriber) = open_channel_for_one_link(&format!("member/{member}"), 4);
    mailboxes.add_port("in", 16, ReadMode::ReadNextInOrder);
    let member_taken_at_the_read = Some(MemberTakenAtTheRead::of(&a_reading(), member).unwrap());
    mailboxes.add_channel_subscriber(
        "in",
        "L-member",
        &InboundLinkName::from("psensor/reading"),
        subscriber,
        member_taken_at_the_read,
    );
    publisher
}

fn publish_a_reading(
    publisher: &TestChannelPublisherNumberingItsSends,
    reading: serde_json::Value,
) {
    let packed = pack_pin_value(&a_reading(), &reading, 0).unwrap();
    publisher.send_numbered(&TestChannelSample {
        payload_descriptor: packed.payload_descriptor,
        ..TestChannelSample::carrying(7, &packed.payload)
    });
}

/// Mental-revert: a subscriber that takes no member hands the reader the bundle's Records
/// message, which a Float does not unpack.
#[test]
fn a_member_link_reads_the_members_own_message_with_the_bundles_stamp() {
    let mailboxes = InputMailboxesInner::new();
    let publisher = a_port_reading_a_member(&mailboxes, "level");

    publish_a_reading(&publisher, json!({ "level": 0.25, "label": "kitchen" }));

    let (message, _) = mailboxes
        .read_raw_from_inbound_link("in")
        .unwrap()
        .expect("the member arrives");
    assert_eq!(message.timestamp_ns, 7);
    let read = unpack_pin_value(
        &PinType::Float { exact_size: None },
        PackedPinValueRef {
            payload_descriptor: &message.payload_descriptor,
            payload: &message.payload,
        },
    )
    .unwrap();
    assert_eq!(read, json!(0.25));
}

#[test]
fn a_member_the_message_leaves_out_reads_its_default() {
    let mailboxes = InputMailboxesInner::new();
    let publisher = a_port_reading_a_member(&mailboxes, "label");

    publish_a_reading(&publisher, json!({ "level": 0.5 }));

    let (message, _) = mailboxes
        .read_raw_from_inbound_link("in")
        .unwrap()
        .expect("the member arrives");
    let read = unpack_pin_value(
        &PinType::String {},
        PackedPinValueRef {
            payload_descriptor: &message.payload_descriptor,
            payload: &message.payload,
        },
    )
    .unwrap();
    assert_eq!(read, json!("unnamed"));
}

/// A message the member cannot be taken from is dropped and counted against its link, never
/// handed to the reader as the member.
#[test]
fn a_message_holding_no_bundle_is_dropped_and_counted() {
    let mailboxes = InputMailboxesInner::new();
    let publisher = a_port_reading_a_member(&mailboxes, "level");

    publish_one_frame(&publisher, "reading", b"not a records message");

    assert!(
        mailboxes
            .read_raw_from_inbound_link("in")
            .unwrap()
            .is_none()
    );
    assert_eq!(dropped_bag_counts_of(&mailboxes).get("L-member"), Some(&1));
}
