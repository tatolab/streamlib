// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A link that starts at one member of a bundle hands its reader that member's own message,
//! taken from each message of the whole bundle as it arrives.

use serde_json::json;

use super::*;
use crate::core::descriptors::{BundleMember, PinType};
use crate::core::pin_value_packing::{
    HeavyMemberOfABundleFrame, PackedPinValueRef, pack_pin_value, the_bundle_frame_of_a_bundle,
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

/// A port fed by one link that takes `member` of a reading at the read, and its publisher.
fn a_port_reading_a_member(
    mailboxes: &InputMailboxesInner,
    member: &str,
) -> TestChannelPublisherNumberingItsSends {
    a_port_reading_a_member_of(mailboxes, &a_reading(), member)
}

/// A port fed by one link that takes `member` of `bundle` at the read, and its publisher.
fn a_port_reading_a_member_of(
    mailboxes: &InputMailboxesInner,
    bundle: &PinType,
    member: &str,
) -> TestChannelPublisherNumberingItsSends {
    let (publisher, subscriber) = open_channel_for_one_link(&format!("member/{member}"), 4);
    mailboxes.add_port("in", 16, ReadMode::ReadNextInOrder);
    let bundle_members_at_the_read = BundleMembersAtTheRead {
        taken_from_its_output: Some(MemberTakenAtTheRead::of(bundle, member).unwrap()),
        put_into_its_input: None,
    };
    mailboxes.add_channel_subscriber(
        "in",
        "L-member",
        &InboundLinkName::from("psensor/reading"),
        subscriber,
        bundle_members_at_the_read,
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

/// A picture with the level metered off it and the sound recorded with it.
fn a_metered_picture() -> PinType {
    PinType::Bundle {
        members: vec![
            a_bundle_member("picture", a_texture(), None),
            a_bundle_member("level", PinType::Float { exact_size: None }, None),
            a_bundle_member("sound", PinType::Audio {}, None),
        ],
    }
}

/// Publish one Bundle frame of a metered picture: level 0.5, the picture and the sound.
fn publish_a_metered_picture(publisher: &TestChannelPublisherNumberingItsSends) {
    let samples = the_sound_samples();
    let packed = the_bundle_frame_of_a_bundle(
        &a_metered_picture(),
        &json!({ "level": 0.5 }),
        &[
            HeavyMemberOfABundleFrame {
                name: "picture",
                descriptor: an_image_frame(4, 19),
                payload: &[],
            },
            HeavyMemberOfABundleFrame {
                name: "sound",
                descriptor: a_sound_block(),
                payload: &samples,
            },
        ],
    )
    .unwrap();
    publisher.send_numbered(&TestChannelSample {
        payload_descriptor: packed.payload_descriptor,
        ..TestChannelSample::carrying(11, &packed.payload)
    });
}

/// A link from the Texture member of a Bundle frame reads the picture's own Image frame, naming
/// the pool slot the frame did, with the frame's stamp.
///
/// Mental-revert: a frame refused at the read drops every message, so the reader reads nothing.
#[test]
fn a_texture_member_link_reads_the_pictures_own_image_frame() {
    let mailboxes = InputMailboxesInner::new();
    let publisher = a_port_reading_a_member_of(&mailboxes, &a_metered_picture(), "picture");

    publish_a_metered_picture(&publisher);

    let (message, _) = mailboxes
        .read_raw_from_inbound_link("in")
        .unwrap()
        .expect("the picture arrives");
    assert_eq!(message.timestamp_ns, 11);
    assert_eq!(message.payload_descriptor, an_image_frame(4, 19));
    assert!(message.payload.is_empty());
}

/// A number member of a frame holding a Texture is taken as it is from a bundle of small members.
///
/// Mental-revert: refusing a Bundle frame at the read drops the number with the picture.
#[test]
fn a_number_member_link_of_a_frame_holding_a_texture_reads_the_number() {
    let mailboxes = InputMailboxesInner::new();
    let publisher = a_port_reading_a_member_of(&mailboxes, &a_metered_picture(), "level");

    publish_a_metered_picture(&publisher);

    let (message, _) = mailboxes
        .read_raw_from_inbound_link("in")
        .unwrap()
        .expect("the level arrives");
    let read = unpack_pin_value(
        &PinType::Float { exact_size: None },
        PackedPinValueRef {
            payload_descriptor: &message.payload_descriptor,
            payload: &message.payload,
        },
    )
    .unwrap();
    assert_eq!(read, json!(0.5));
    assert_eq!(dropped_bag_counts_of(&mailboxes).get("L-member"), Some(&0));
}

#[test]
fn an_audio_member_link_reads_the_block_with_its_samples() {
    let mailboxes = InputMailboxesInner::new();
    let publisher = a_port_reading_a_member_of(&mailboxes, &a_metered_picture(), "sound");

    publish_a_metered_picture(&publisher);

    let (message, _) = mailboxes
        .read_raw_from_inbound_link("in")
        .unwrap()
        .expect("the sound arrives");
    assert_eq!(message.payload_descriptor, a_sound_block());
    assert_eq!(message.payload.bytes(), the_sound_samples().as_slice());
}
