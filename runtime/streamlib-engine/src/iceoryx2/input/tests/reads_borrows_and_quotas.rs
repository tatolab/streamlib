// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use crate::iceoryx2::input_port_borrow_quota::DEFAULT_INPUT_PORT_BORROW_QUOTA;

/// A message written as bytes reaches `read_raw` as its producer wrote it:
/// the payload untouched, its stamp, and its group flag cleared then set.
#[test]
fn a_message_written_as_bytes_reaches_read_raw_with_its_stamp_and_group_flag() {
    let channel = crate::iceoryx2::Iceoryx2Node::for_this_test_process()
        .open_or_create_service(&unique_suffix("bytes-to-read-raw"), 2, 16)
        .unwrap();
    let mailboxes = InputMailboxesInner::new();
    mailboxes.add_port("in", 16, ReadMode::ReadNextInOrder);
    mailboxes.add_channel_subscriber(
        "in",
        "L-bytes",
        &InboundLinkName::from("psource/out"),
        channel.create_subscriber(16).unwrap(),
    );
    let output_writer = output_writer_publishing_onto(&channel, "out");

    output_writer
        .write_bytes_with_timestamp(
            "out",
            b"continues-a-group",
            111,
            MessageGroupBoundary::ContinuesGroup,
        )
        .unwrap();
    output_writer
        .write_bytes_with_timestamp(
            "out",
            b"starts-a-group",
            222,
            MessageGroupBoundary::StartsGroup,
        )
        .unwrap();

    assert_eq!(
        mailboxes.read_raw("in").unwrap(),
        Some(MessageReadFromInputPort {
            payload: b"continues-a-group".to_vec().into(),
            timestamp_ns: 111,
            group_boundary: MessageGroupBoundary::ContinuesGroup,
            lost_since_last_read: 0,
            payload_descriptor: DataChannelPayloadDescriptor::bytes(),
        })
    );
    assert_eq!(
        mailboxes.read_raw_from_inbound_link("in").unwrap(),
        Some((
            MessageReadFromInputPort {
                payload: b"starts-a-group".to_vec().into(),
                timestamp_ns: 222,
                group_boundary: MessageGroupBoundary::StartsGroup,
                lost_since_last_read: 0,
                payload_descriptor: DataChannelPayloadDescriptor::bytes(),
            },
            InboundLinkName::from("psource/out"),
        ))
    );
    assert_eq!(mailboxes.read_raw("in").unwrap(), None);
}

/// A read hands back the channel sample itself, and the bytes it borrows
/// stay as they were while the publisher keeps sending into the same
/// channel.
///
/// Fail-without-fix: copy the sample out at the receive and the payload is
/// owned bytes, not the borrowed sample.
#[test]
fn a_borrowed_message_keeps_its_bytes_while_the_publisher_keeps_sending() {
    let (mailboxes, output_writer) =
        a_port_read_as_fed_by_a_native_writer("borrow-held", ReadMode::ReadNextInOrder);
    write_starting_a_group(&output_writer, "held");
    let held = mailboxes
        .read_raw("in")
        .unwrap()
        .expect("the first message");
    assert!(
        matches!(
            held.payload,
            InputPortMessagePayload::BorrowedChannelSample(_)
        ),
        "a read must borrow the sample, not copy it: {:?}",
        held.payload
    );
    let held_address = held.payload.as_ptr();
    assert_eq!(
        held_address as usize % 64,
        0,
        "the payload starts cache-line aligned"
    );

    for later in 0..40 {
        write_starting_a_group(&output_writer, &format!("later-{later:02}"));
        let _ = mailboxes.read_raw("in").unwrap();
    }

    assert_eq!(held.payload, b"held");
    assert_eq!(held.payload.as_ptr(), held_address);
}

/// A reader holding its quota past a full mailbox drops and counts only its
/// own link's newest frames, and a second link on the same output, read as
/// it arrives, receives every frame.
///
/// Fail-without-fix: hold no per-link budget and the stalled reader's
/// subscriber exhausts its borrow limit, so its frames are lost uncounted
/// in its ring.
#[test]
fn a_link_past_its_quota_drops_only_its_own_frames() {
    const MAILBOX_DEPTH: usize = 4;
    const FRAMES_PAST_THE_FULL_MAILBOX: usize = 5;
    let channel = crate::iceoryx2::Iceoryx2Node::for_this_test_process()
        .open_or_create_service(&unique_suffix("quota-two-links"), 3, 16)
        .unwrap();
    let stalled = InputMailboxesInner::new();
    stalled.add_port("in", MAILBOX_DEPTH, ReadMode::ReadNextInOrder);
    stalled.add_channel_subscriber(
        "in",
        "L-stalled",
        &InboundLinkName::from("psource/out"),
        channel.create_subscriber(16).unwrap(),
    );
    let keeping_up = InputMailboxesInner::new();
    keeping_up.add_port("in", MAILBOX_DEPTH, ReadMode::ReadNextInOrder);
    keeping_up.add_channel_subscriber(
        "in",
        "L-keeping-up",
        &InboundLinkName::from("psource/out"),
        channel.create_subscriber(16).unwrap(),
    );
    let output_writer = output_writer_publishing_onto(&channel, "out");
    let quota = DEFAULT_INPUT_PORT_BORROW_QUOTA;

    let mut held_by_the_stalled_reader = Vec::new();
    let mut read_by_the_keeping_up_reader = 0;
    let total_frames = quota + MAILBOX_DEPTH + FRAMES_PAST_THE_FULL_MAILBOX;
    for frame in 0..total_frames {
        write_starting_a_group(&output_writer, &format!("frame-{frame}"));
        if held_by_the_stalled_reader.len() < quota {
            held_by_the_stalled_reader
                .push(stalled.read_raw("in").unwrap().expect("a frame to hold"));
        } else {
            stalled.receive_pending();
        }
        while keeping_up.read_raw("in").unwrap().is_some() {
            read_by_the_keeping_up_reader += 1;
        }
    }

    assert_eq!(
        dropped_bag_counts_of(&stalled),
        std::collections::BTreeMap::from([(
            "L-stalled".to_string(),
            FRAMES_PAST_THE_FULL_MAILBOX as u64
        )]),
    );
    assert_eq!(
        dropped_bag_counts_of(&keeping_up),
        std::collections::BTreeMap::from([("L-keeping-up".to_string(), 0)]),
    );
    assert_eq!(read_by_the_keeping_up_reader, total_frames);

    drop(held_by_the_stalled_reader);
    write_starting_a_group(&output_writer, "after-the-release");
    stalled.receive_pending();
    let mut queued = stalled.drain("in");
    assert_eq!(queued.len(), MAILBOX_DEPTH);
    assert_eq!(queued.pop().unwrap(), b"after-the-release");
}

/// A port's quota is what it was set to: with a quota of zero, a message its
/// reader holds takes a place in the mailbox, so the frame that would fill
/// it is the newest one dropped.
#[test]
fn a_port_with_no_quota_drops_the_newest_frame_while_its_reader_holds_one() {
    let (mailboxes, output_writer) =
        a_port_read_as_fed_by_a_native_writer("quota-zero", ReadMode::ReadNextInOrder);
    mailboxes.set_input_port_borrow_quota("in", 0).unwrap();
    write_starting_a_group(&output_writer, "held");
    let _held = mailboxes.read_raw("in").unwrap().expect("the held message");
    for frame in 0..15 {
        write_starting_a_group(&output_writer, &format!("queued-{frame}"));
    }
    write_starting_a_group(&output_writer, "past-the-quota");
    mailboxes.receive_pending();

    assert_eq!(
        dropped_bag_counts_of(&mailboxes),
        std::collections::BTreeMap::from([("L-declared".to_string(), 1)]),
    );
    assert!(
        mailboxes
            .set_input_port_borrow_quota("not-a-port", 1)
            .is_err()
    );
}

/// Groups a writer declared given up upstream reach an `ordered` reader on
/// the next message as `lost_since_last_read`, and never as the link's
/// drop: the link lost nothing.
#[test]
fn a_declared_upstream_loss_reaches_lost_since_last_read_on_an_ordered_port() {
    let (mailboxes, output_writer) =
        a_port_read_as_fed_by_a_native_writer("declared/ordered", ReadMode::ReadNextInOrder);

    write_starting_a_group(&output_writer, "before");
    output_writer
        .declare_groups_given_up_upstream_before_the_next_write("out", 2)
        .unwrap();
    write_starting_a_group(&output_writer, "after");
    write_starting_a_group(&output_writer, "later");

    assert_eq!(
        payloads_and_losses_read_off(&mailboxes, "in"),
        [("before", 0), ("after", 2), ("later", 0)]
            .map(|(payload, lost)| (payload.to_string(), lost)),
    );
    assert_eq!(
        dropped_bag_counts_of(&mailboxes),
        std::collections::BTreeMap::from([("L-declared".to_string(), 0)]),
        "a loss upstream of the writer is never the link's drop"
    );
}

/// A link's first sample is its numbering's baseline, yet a loss declared
/// on it is told: the count rides its own header field.
#[test]
fn a_declared_upstream_loss_on_a_links_first_sample_is_still_counted() {
    let (mailboxes, output_writer) =
        a_port_read_as_fed_by_a_native_writer("declared/first", ReadMode::ReadNextInOrder);

    output_writer
        .declare_groups_given_up_upstream_before_the_next_write("out", 1)
        .unwrap();
    write_starting_a_group(&output_writer, "first");

    assert_eq!(
        payloads_and_losses_read_off(&mailboxes, "in"),
        [("first".to_string(), 1)],
    );
}

/// A `newest` port passing over messages is the profile working, so a loss
/// declared upstream is not one either.
#[test]
fn a_newest_port_does_not_count_a_declared_upstream_loss() {
    let (mailboxes, output_writer) =
        a_port_read_as_fed_by_a_native_writer("declared/newest", ReadMode::SkipToLatest);

    output_writer
        .declare_groups_given_up_upstream_before_the_next_write("out", 3)
        .unwrap();
    write_starting_a_group(&output_writer, "latest");

    assert_eq!(
        payloads_and_losses_read_off(&mailboxes, "in"),
        [("latest".to_string(), 0)],
    );
    assert_eq!(
        dropped_bag_counts_of(&mailboxes),
        std::collections::BTreeMap::from([("L-declared".to_string(), 0)]),
    );
}
