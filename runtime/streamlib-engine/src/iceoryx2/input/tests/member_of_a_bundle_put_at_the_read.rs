// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Links that end at members of one bundle input hand its reader one message built from the
//! newest value each has carried, stamped as the message that arrived last.

use serde_json::{Value, json};

use super::*;
use crate::core::descriptors::{BundleMember, PinType};
use crate::core::pin_value_packing::{
    PackedPinValueRef, pack_pin_value, the_members_of_a_bundle_a_records_message_carries,
    unpack_pin_value,
};

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

/// The member type of `member` of a reading.
fn the_type_of(member: &str) -> PinType {
    match member {
        "level" => PinType::Float { exact_size: None },
        _ => PinType::String {},
    }
}

/// A link into `member` of the reading port `in`, built into `put_into`, and its publisher.
fn a_link_into_the_member(
    mailboxes: &InputMailboxesInner,
    member: &str,
    put_into: WhatTheMembersArePutInto,
) -> TestChannelPublisherNumberingItsSends {
    let (publisher, subscriber) = open_channel_for_one_link(&format!("into/{member}"), 4);
    if !mailboxes.has_port("in") {
        mailboxes.add_port("in", 16, ReadMode::ReadNextInOrder);
    }
    let PinType::Bundle { members } = a_reading() else {
        unreachable!("a reading is a bundle")
    };
    let member_declared = members
        .into_iter()
        .find(|declared| declared.name == member)
        .unwrap();
    mailboxes.add_channel_subscriber(
        "in",
        &format!("L-{member}"),
        &InboundLinkName::from(format!("source/{member}").as_str()),
        subscriber,
        BundleMembersAtTheRead {
            taken_from_its_output: None,
            put_into_its_input: Some(MemberPutAtTheRead {
                bundle: a_reading(),
                member: member_declared,
                put_into,
            }),
        },
    );
    publisher
}

fn publish_the_member(
    publisher: &TestChannelPublisherNumberingItsSends,
    member: &str,
    value: Value,
    timestamp_ns: i64,
) {
    let packed = pack_pin_value(&the_type_of(member), &value, 0).unwrap();
    publisher.send_numbered(&TestChannelSample {
        payload_descriptor: packed.payload_descriptor,
        ..TestChannelSample::carrying(timestamp_ns, &packed.payload)
    });
}

/// Each message `in` hands its reader, as the bundle it carries and its stamp.
fn the_bundles_read(mailboxes: &InputMailboxesInner) -> Vec<(Value, i64)> {
    std::iter::from_fn(|| mailboxes.read_raw_from_inbound_link("in").unwrap())
        .map(|(message, _)| {
            let bundle = unpack_pin_value(
                &a_reading(),
                PackedPinValueRef {
                    payload_descriptor: &message.payload_descriptor,
                    payload: &message.payload,
                },
            )
            .unwrap();
            (bundle, message.timestamp_ns)
        })
        .collect()
}

/// Mental-revert: a port that took each member's message as it came would hand its reader a
/// Float and a String, neither of which unpacks as the reading.
#[test]
fn the_reader_takes_one_bundle_of_the_newest_value_of_each_member() {
    let mailboxes = InputMailboxesInner::new();
    let into_level = a_link_into_the_member(
        &mailboxes,
        "level",
        WhatTheMembersArePutInto::TheWholeBundle,
    );
    let into_label = a_link_into_the_member(
        &mailboxes,
        "label",
        WhatTheMembersArePutInto::TheWholeBundle,
    );

    let mut read = Vec::new();
    publish_the_member(&into_level, "level", json!(0.25), 7);
    read.extend(the_bundles_read(&mailboxes));
    publish_the_member(&into_label, "label", json!("kitchen"), 9);
    read.extend(the_bundles_read(&mailboxes));
    publish_the_member(&into_level, "level", json!(0.75), 11);
    read.extend(the_bundles_read(&mailboxes));

    assert_eq!(
        read,
        vec![
            (json!({ "level": 0.25, "label": "unnamed" }), 7),
            (json!({ "level": 0.25, "label": "kitchen" }), 9),
            (json!({ "level": 0.75, "label": "kitchen" }), 11),
        ]
    );
}

#[test]
fn no_bundle_is_handed_until_each_member_without_a_default_has_arrived() {
    let mailboxes = InputMailboxesInner::new();
    let into_level = a_link_into_the_member(
        &mailboxes,
        "level",
        WhatTheMembersArePutInto::TheWholeBundle,
    );
    let into_label = a_link_into_the_member(
        &mailboxes,
        "label",
        WhatTheMembersArePutInto::TheWholeBundle,
    );

    publish_the_member(&into_label, "label", json!("kitchen"), 3);
    assert_eq!(the_bundles_read(&mailboxes), Vec::new());

    publish_the_member(&into_level, "level", json!(0.5), 5);
    assert_eq!(
        the_bundles_read(&mailboxes),
        vec![(json!({ "level": 0.5, "label": "kitchen" }), 5)]
    );
    assert_eq!(dropped_bag_counts_of(&mailboxes).get("L-label"), Some(&0));
}

#[test]
fn an_island_is_handed_the_members_the_links_carried_alone() {
    let mailboxes = InputMailboxesInner::new();
    let into_label = a_link_into_the_member(
        &mailboxes,
        "label",
        WhatTheMembersArePutInto::TheMembersTheLinksCarried,
    );

    publish_the_member(&into_label, "label", json!("kitchen"), 3);

    let (message, _) = mailboxes
        .read_raw_from_inbound_link("in")
        .unwrap()
        .expect("the members arrive");
    let members = the_members_of_a_bundle_a_records_message_carries(
        &a_reading(),
        PackedPinValueRef {
            payload_descriptor: &message.payload_descriptor,
            payload: &message.payload,
        },
    )
    .unwrap();
    assert_eq!(Value::Object(members), json!({ "label": "kitchen" }));
}

/// Mental-revert: a port that kept a removed link's member would go on handing its reader the
/// last label that link carried.
#[test]
fn a_member_link_removed_takes_its_member_out_of_the_bundle() {
    let mailboxes = InputMailboxesInner::new();
    let into_level = a_link_into_the_member(
        &mailboxes,
        "level",
        WhatTheMembersArePutInto::TheWholeBundle,
    );
    let into_label = a_link_into_the_member(
        &mailboxes,
        "label",
        WhatTheMembersArePutInto::TheWholeBundle,
    );
    publish_the_member(&into_label, "label", json!("kitchen"), 3);
    publish_the_member(&into_level, "level", json!(0.5), 5);
    the_bundles_read(&mailboxes);

    mailboxes.remove_channel_link("L-label");
    publish_the_member(&into_level, "level", json!(0.6), 7);

    assert_eq!(
        the_bundles_read(&mailboxes),
        vec![(json!({ "level": 0.6, "label": "unnamed" }), 7)]
    );
}

#[test]
fn a_message_holding_no_value_of_the_member_is_dropped_and_counted() {
    let mailboxes = InputMailboxesInner::new();
    let into_level = a_link_into_the_member(
        &mailboxes,
        "level",
        WhatTheMembersArePutInto::TheWholeBundle,
    );

    publish_one_frame(&into_level, "level", b"not a control message");

    assert_eq!(the_bundles_read(&mailboxes), Vec::new());
    assert_eq!(dropped_bag_counts_of(&mailboxes).get("L-level"), Some(&1));
}

/// A member read refuses what a whole-port read refuses: a message stamping more payload than
/// its sample carries is dropped and counted, not read as the member it starts with.
///
/// Mental-revert: falling back to the whole sample hands the reader a bundle with `level` 0.5.
#[test]
fn a_message_stamping_more_payload_than_its_sample_carries_is_dropped_and_counted() {
    let mailboxes = InputMailboxesInner::new();
    let into_level = a_link_into_the_member(
        &mailboxes,
        "level",
        WhatTheMembersArePutInto::TheWholeBundle,
    );
    let packed = pack_pin_value(&the_type_of("level"), &json!(0.5), 0).unwrap();
    let stamped_past_the_sample = u32::try_from(packed.payload.len() + 64).unwrap();

    into_level.send_numbered(&TestChannelSample {
        payload_descriptor: packed.payload_descriptor,
        ..TestChannelSample::stamping(5, stamped_past_the_sample, &packed.payload)
    });

    assert_eq!(the_bundles_read(&mailboxes), Vec::new());
    assert_eq!(dropped_bag_counts_of(&mailboxes).get("L-level"), Some(&1));
}
