// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Links that end at members of one bundle input hand its reader one message built from the
//! newest value each has carried, stamped as the message that arrived last.

use serde_json::{Value, json};
use streamlib_ipc_types::DataChannelPayloadDescriptor;

use super::*;
use crate::core::descriptors::PinType;
use crate::core::pin_value_packing::{
    PackedPinValueRef, pack_pin_value, the_bundle_a_bundle_frame_carries,
    the_members_of_a_bundle_a_records_message_carries, unpack_pin_value,
};

fn a_reading() -> PinType {
    PinType::Bundle {
        name: None,
        members: vec![
            a_bundle_member("level", PinType::Float { exact_size: None }, None),
            a_bundle_member("label", PinType::String {}, Some(json!("unnamed"))),
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
    a_link_into_a_member_of(mailboxes, &a_reading(), member, put_into)
}

/// A link into `member` of port `in`, which reads `bundle` whole or as `put_into` says, and its
/// publisher.
fn a_link_into_a_member_of(
    mailboxes: &InputMailboxesInner,
    bundle: &PinType,
    member: &str,
    put_into: WhatTheMembersArePutInto,
) -> TestChannelPublisherNumberingItsSends {
    let (publisher, subscriber) = open_channel_for_one_link(&format!("into/{member}"), 4);
    if !mailboxes.has_port("in") {
        mailboxes.add_port("in", 16, ReadMode::ReadNextInOrder);
    }
    let PinType::Bundle { members, .. } = bundle.clone() else {
        unreachable!("the input reads a bundle")
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
                bundle: bundle.clone(),
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

/// A member no link carries reads the value set on the input, as an unlinked input reads its
/// own, and a link into it carries over that value once it arrives.
///
/// Mental-revert: a port reading only what the links carried would hand no bundle until a
/// `level` arrives, as `level` declares no default.
#[test]
fn a_member_no_link_has_carried_reads_the_value_set_on_the_input() {
    let mailboxes = InputMailboxesInner::new();
    let into_label = a_link_into_the_member(
        &mailboxes,
        "label",
        WhatTheMembersArePutInto::TheWholeBundle,
    );
    let into_level = a_link_into_the_member(
        &mailboxes,
        "level",
        WhatTheMembersArePutInto::TheWholeBundle,
    );
    let set = json!({ "in": { "level": 0.4, "label": "set", "gone": 1 } });
    mailboxes.read_each_unlinked_input_as_its_value(set.as_object().unwrap());

    publish_the_member(&into_label, "label", json!("kitchen"), 3);
    let read_before_a_level = the_bundles_read(&mailboxes);
    publish_the_member(&into_level, "level", json!(0.9), 5);

    assert_eq!(
        read_before_a_level,
        vec![(json!({ "level": 0.4, "label": "kitchen" }), 3)]
    );
    assert_eq!(
        the_bundles_read(&mailboxes),
        vec![(json!({ "level": 0.9, "label": "kitchen" }), 5)]
    );
}

/// A picture with the level metered off it: a Texture beside a Float.
fn a_metered_picture() -> PinType {
    PinType::Bundle {
        name: None,
        members: vec![
            a_bundle_member("picture", a_texture(), None),
            a_bundle_member("level", PinType::Float { exact_size: None }, None),
        ],
    }
}

fn publish_a_level(publisher: &TestChannelPublisherNumberingItsSends, level: f64, stamp: i64) {
    let packed = pack_pin_value(&PinType::Float { exact_size: None }, &json!(level), 0).unwrap();
    publish_a_message(publisher, packed.payload_descriptor, &packed.payload, stamp);
}

/// Each Bundle frame `in` hands its reader, read as `bundle`: its small members, each heavy
/// member's descriptor and bytes by name, and its stamp.
fn the_bundle_frames_read(
    mailboxes: &InputMailboxesInner,
    bundle: &PinType,
) -> Vec<(
    Value,
    Vec<(String, DataChannelPayloadDescriptor, Vec<u8>)>,
    i64,
)> {
    std::iter::from_fn(|| mailboxes.read_raw_from_inbound_link("in").unwrap())
        .map(|(message, _)| {
            let read = the_bundle_a_bundle_frame_carries(
                bundle,
                PackedPinValueRef {
                    payload_descriptor: &message.payload_descriptor,
                    payload: &message.payload,
                },
            )
            .unwrap();
            let heavy_members = read
                .heavy_members
                .iter()
                .map(|heavy| (heavy.name.clone(), heavy.descriptor, heavy.payload.to_vec()))
                .collect();
            (
                Value::Object(read.small_members),
                heavy_members,
                message.timestamp_ns,
            )
        })
        .collect()
}

fn the_picture(
    descriptor: DataChannelPayloadDescriptor,
) -> (String, DataChannelPayloadDescriptor, Vec<u8>) {
    ("picture".to_owned(), descriptor, Vec::new())
}

/// A Texture wired into one member and a number into another arrive as one Bundle frame,
/// the picture by the pool slot it names, the number beside it, stamped as the message that
/// arrived last.
///
/// Mental-revert: unpacking the Texture's message as a stored value refuses it, so the port
/// drops every picture and hands its reader nothing.
#[test]
fn a_texture_and_a_number_wired_into_members_arrive_as_one_bundle_frame() {
    let mailboxes = InputMailboxesInner::new();
    let into_picture = a_link_into_a_member_of(
        &mailboxes,
        &a_metered_picture(),
        "picture",
        WhatTheMembersArePutInto::TheWholeBundle,
    );
    let into_level = a_link_into_a_member_of(
        &mailboxes,
        &a_metered_picture(),
        "level",
        WhatTheMembersArePutInto::TheWholeBundle,
    );

    publish_a_level(&into_level, 0.5, 3);
    let read_before_a_picture = the_bundle_frames_read(&mailboxes, &a_metered_picture());
    publish_a_message(&into_picture, an_image_frame(4, 19), &[], 5);
    let read_with_the_picture = the_bundle_frames_read(&mailboxes, &a_metered_picture());
    publish_a_level(&into_level, 0.75, 7);

    assert_eq!(read_before_a_picture, Vec::new());
    assert_eq!(
        read_with_the_picture,
        vec![(
            json!({ "level": 0.5 }),
            vec![the_picture(an_image_frame(4, 19))],
            5
        )]
    );
    assert_eq!(
        the_bundle_frames_read(&mailboxes, &a_metered_picture()),
        vec![(
            json!({ "level": 0.75 }),
            vec![the_picture(an_image_frame(4, 19))],
            7
        )]
    );
    assert_eq!(dropped_bag_counts_of(&mailboxes).get("L-picture"), Some(&0));
}

/// A number set on the input, beside a picture wired into its member, rides each Bundle frame
/// the picture's messages make, as the value a member no link has carried reads.
///
/// Mental-revert: a port reading only what the links carried hands no frame, as `level`
/// declares no default and no link carries it.
#[test]
fn a_number_set_beside_a_wired_texture_rides_each_bundle_frame() {
    let mailboxes = InputMailboxesInner::new();
    let into_picture = a_link_into_a_member_of(
        &mailboxes,
        &a_metered_picture(),
        "picture",
        WhatTheMembersArePutInto::TheWholeBundle,
    );
    let set = json!({ "in": { "level": 0.4 } });
    mailboxes.read_each_unlinked_input_as_its_value(set.as_object().unwrap());

    publish_a_message(&into_picture, an_image_frame(4, 19), &[], 5);

    assert_eq!(
        the_bundle_frames_read(&mailboxes, &a_metered_picture()),
        vec![(
            json!({ "level": 0.4 }),
            vec![the_picture(an_image_frame(4, 19))],
            5
        )]
    );
}

/// A newer picture replaces the one the port holds, so the bundle never names a slot its
/// producer has since handed out again, and the port holds one picture however many arrive.
///
/// Mental-revert: keeping the first picture held hands the reader slot 4 at generation 19 after
/// generation 20 arrived.
#[test]
fn a_newer_texture_replaces_the_one_the_port_holds() {
    let mailboxes = InputMailboxesInner::new();
    let into_picture = a_link_into_a_member_of(
        &mailboxes,
        &a_metered_picture(),
        "picture",
        WhatTheMembersArePutInto::TheWholeBundle,
    );
    let into_level = a_link_into_a_member_of(
        &mailboxes,
        &a_metered_picture(),
        "level",
        WhatTheMembersArePutInto::TheWholeBundle,
    );
    publish_a_level(&into_level, 0.5, 3);
    for generation in 19..=20 {
        publish_a_message(&into_picture, an_image_frame(4, generation), &[], 5);
    }

    let read = the_bundle_frames_read(&mailboxes, &a_metered_picture());

    assert_eq!(
        read.last().map(|(_, heavy, _)| heavy.clone()),
        Some(vec![the_picture(an_image_frame(4, 20))])
    );
}

/// The member a removed link carried is forgotten, a picture as a number is, so the port holds
/// no picture past its link and hands no bundle without one.
///
/// Mental-revert: forgetting only the small members keeps the last picture held and hands it on
/// with the next level.
#[test]
fn a_texture_link_removed_takes_its_picture_out_of_the_port() {
    let mailboxes = InputMailboxesInner::new();
    let into_picture = a_link_into_a_member_of(
        &mailboxes,
        &a_metered_picture(),
        "picture",
        WhatTheMembersArePutInto::TheWholeBundle,
    );
    let into_level = a_link_into_a_member_of(
        &mailboxes,
        &a_metered_picture(),
        "level",
        WhatTheMembersArePutInto::TheWholeBundle,
    );
    publish_a_message(&into_picture, an_image_frame(4, 19), &[], 3);
    publish_a_level(&into_level, 0.5, 5);
    the_bundle_frames_read(&mailboxes, &a_metered_picture());

    mailboxes.remove_channel_link("L-picture");
    publish_a_level(&into_level, 0.6, 7);

    assert_eq!(
        the_bundle_frames_read(&mailboxes, &a_metered_picture()),
        Vec::new()
    );
}

/// A message on a Texture member's link that is no Image frame is dropped and counted, and the
/// picture held before it stays.
#[test]
fn a_message_of_another_kind_into_a_texture_member_is_dropped_and_counted() {
    let mailboxes = InputMailboxesInner::new();
    let into_picture = a_link_into_a_member_of(
        &mailboxes,
        &a_metered_picture(),
        "picture",
        WhatTheMembersArePutInto::TheWholeBundle,
    );
    let into_level = a_link_into_a_member_of(
        &mailboxes,
        &a_metered_picture(),
        "level",
        WhatTheMembersArePutInto::TheWholeBundle,
    );
    publish_a_message(&into_picture, an_image_frame(4, 19), &[], 3);
    publish_one_frame(&into_picture, "picture", b"no picture");
    publish_a_level(&into_level, 0.5, 5);

    assert_eq!(
        the_bundle_frames_read(&mailboxes, &a_metered_picture()),
        vec![(
            json!({ "level": 0.5 }),
            vec![the_picture(an_image_frame(4, 19))],
            5
        )]
    );
    assert_eq!(dropped_bag_counts_of(&mailboxes).get("L-picture"), Some(&1));
}

/// An Audio member rides its own row of the frame, its samples copied as its link carried them.
#[test]
fn an_audio_block_wired_into_a_member_rides_the_bundle_frame_with_its_samples() {
    let a_sound_with_its_label = PinType::Bundle {
        name: None,
        members: vec![
            a_bundle_member("sound", PinType::Audio {}, None),
            a_bundle_member("label", PinType::String {}, Some(json!("unnamed"))),
        ],
    };
    let mailboxes = InputMailboxesInner::new();
    let into_sound = a_link_into_a_member_of(
        &mailboxes,
        &a_sound_with_its_label,
        "sound",
        WhatTheMembersArePutInto::TheWholeBundle,
    );
    let block = a_sound_block();
    let samples = the_sound_samples();

    publish_a_message(&into_sound, block, &samples, 9);

    assert_eq!(
        the_bundle_frames_read(&mailboxes, &a_sound_with_its_label),
        vec![(
            json!({ "label": "unnamed" }),
            vec![("sound".to_owned(), block, samples)],
            9
        )]
    );
}

/// A Texture member stating a default still waits for its picture: a default never stands in
/// for a Texture, so a number arriving first builds no bundle, and is no loss either.
///
/// Mental-revert: reading the Texture's default as the member having arrived packs a frame
/// lacking the picture's row, which is refused, so every number is dropped and counted.
#[test]
fn a_defaulted_texture_member_not_yet_arrived_builds_no_bundle_and_drops_nothing() {
    let a_picture_defaulted = PinType::Bundle {
        name: None,
        members: vec![
            a_bundle_member("picture", a_texture(), Some(json!({}))),
            a_bundle_member("level", PinType::Float { exact_size: None }, None),
        ],
    };
    let mailboxes = InputMailboxesInner::new();
    let into_picture = a_link_into_a_member_of(
        &mailboxes,
        &a_picture_defaulted,
        "picture",
        WhatTheMembersArePutInto::TheWholeBundle,
    );
    let into_level = a_link_into_a_member_of(
        &mailboxes,
        &a_picture_defaulted,
        "level",
        WhatTheMembersArePutInto::TheWholeBundle,
    );

    publish_a_level(&into_level, 0.5, 3);
    let read_before_a_picture = the_bundle_frames_read(&mailboxes, &a_picture_defaulted);
    publish_a_message(&into_picture, an_image_frame(4, 19), &[], 5);

    assert_eq!(read_before_a_picture, Vec::new());
    assert_eq!(dropped_bag_counts_of(&mailboxes).get("L-level"), Some(&0));
    assert_eq!(
        the_bundle_frames_read(&mailboxes, &a_picture_defaulted),
        vec![(
            json!({ "level": 0.5 }),
            vec![the_picture(an_image_frame(4, 19))],
            5
        )]
    );
}
