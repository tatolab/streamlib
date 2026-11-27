// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

/// A pool slot resource nothing in this process holds, so its ring rehands
/// it as soon as its turn comes round.
struct PoolSlotNothingInThisProcessHolds;

impl crate::core::context::texture_pools_and_rings::lease_aware_pool_slot_ring::LeaseAwarePoolSlotResource
    for PoolSlotNothingInThisProcessHolds
{
    fn is_held_in_this_process(&self) -> bool {
        false
    }
}

/// A consumer held until its producer has lapped a three-slot output ring
/// reads the oldest queued frame, finds its id recycled at the claim, and
/// counts it lost: one drop on the link `graph` renders, told to the next
/// read from that link, and nothing raised.
///
/// Fail-without-fix: with no read-side count, the recycled frame is lost
/// with the link reporting no drops and the next read told of none.
#[test]
fn a_frame_its_producer_recycled_before_the_read_is_counted_lost_on_its_link() {
    use crate::core::context::texture_pools_and_rings::lease_aware_pool_slot_ring::{
        ANY_ONE_UNHELD_SLOT_IS_REUSABLE, LeaseAwarePoolMintedFrameGenerations,
        LeaseAwarePoolSlotRing,
    };

    let mut producer_output_ring = LeaseAwarePoolSlotRing::default();
    for pool_slot_key in ["slot-a", "slot-b", "slot-c"] {
        producer_output_ring
            .push_fresh_slot(pool_slot_key.to_string(), PoolSlotNothingInThisProcessHolds);
    }
    let minted_frame_generations = LeaseAwarePoolMintedFrameGenerations::default();
    let (mailboxes, output_writer) = a_port_read_as_fed_by_a_native_writer(
        "recycled-before-the-read",
        ReadMode::ReadNextInOrder,
    );

    for _ in 0..=producer_output_ring.slot_count() {
        let slot_index = producer_output_ring
            .hand_off_a_reusable_slot(
                ANY_ONE_UNHELD_SLOT_IS_REUSABLE,
                None,
                &minted_frame_generations,
            )
            .expect("nothing holds a slot, so the producer never waits");
        write_starting_a_group(
            &output_writer,
            &producer_output_ring
                .slot(slot_index)
                .currently_published_frame_id(),
        );
    }

    let read_claimed_and_lost_since_last_read: Vec<(String, bool, u64)> =
        std::iter::from_fn(|| mailboxes.read_raw("in").unwrap())
            .map(|message| {
                let surface_id = String::from_utf8(message.payload.to_vec()).unwrap();
                let claim = minted_frame_generations
                    .refusal_of_a_retired_frame_id_named(&surface_id)
                    .expect("every id names a slot the ring owns");
                let claimed = match claim {
                    Ok(()) => true,
                    Err(Error::SurfaceFrameRecycled { .. }) => {
                        mailboxes
                            .count_the_message_last_read_as_lost("in")
                            .expect("a message a link delivered is countable");
                        false
                    }
                    Err(other) => panic!("only a recycled frame is refused: {other}"),
                };
                (surface_id, claimed, message.lost_since_last_read)
            })
            .collect();

    assert_eq!(
        read_claimed_and_lost_since_last_read,
        [
            ("slot-a#1", false, 0),
            ("slot-b#1", true, 1),
            ("slot-c#1", true, 0),
            ("slot-a#2", true, 0),
        ]
        .map(|(surface_id, claimed, lost)| (surface_id.to_string(), claimed, lost)),
    );
    assert_eq!(
        dropped_bag_counts_of(&mailboxes),
        std::collections::BTreeMap::from([("L-declared".to_string(), 1)]),
    );
}

/// A loss is counted once per message a link delivered: before any read,
/// and a second time for one message, there is nothing to count, and each
/// is refused naming the port.
#[test]
fn counting_a_message_lost_needs_one_a_link_delivered_and_counts_it_once() {
    let (mailboxes, output_writer) =
        a_port_read_as_fed_by_a_native_writer("counted-once", ReadMode::ReadNextInOrder);
    let refusal_before_any_read = mailboxes
        .count_the_message_last_read_as_lost("in")
        .expect_err("nothing has been read");
    assert!(
        refusal_before_any_read.to_string().contains("'in'"),
        "got: {refusal_before_any_read}"
    );

    write_starting_a_group(&output_writer, "only");
    mailboxes.read_raw("in").unwrap().expect("one message");
    mailboxes
        .count_the_message_last_read_as_lost("in")
        .expect("the message a link delivered is countable");
    assert!(
        mailboxes.count_the_message_last_read_as_lost("in").is_err(),
        "one message is one loss"
    );
    assert_eq!(
        dropped_bag_counts_of(&mailboxes),
        std::collections::BTreeMap::from([("L-declared".to_string(), 1)]),
    );
    assert!(matches!(
        mailboxes.count_the_message_last_read_as_lost("not-a-port"),
        Err(Error::Link(refusal)) if refusal.contains("Unknown input port")
    ));
}

/// Bags the ring overwrote behind bags already queued are told to the first
/// bag read after them, never to the queued bags ahead of them.
///
/// Two bags sit in the mailbox when the ring overwrites three more; the read
/// of the first receives the ring's survivors before it pops. Fail-without-fix:
/// tell each read the link's count as it stands at the read, and the first
/// queued bag — which preceded the gap — is told of all three, so a decoder
/// resuming at it decodes across the gap.
#[test]
fn a_ring_gap_behind_queued_bags_is_told_to_the_first_bag_after_it() {
    const RING_DEPTH: usize = 4;
    const BAGS_THE_RING_OVERWROTE: usize = 3;
    let (publisher, subscriber) = open_channel_for_one_link("loss-placement/ring-gap", RING_DEPTH);
    let mailboxes = InputMailboxesInner::new();
    mailboxes.add_port("in", 64, ReadMode::ReadNextInOrder);
    mailboxes.add_channel_subscriber(
        "in",
        "L-ring-gap",
        &InboundLinkName::from("psource/out"),
        subscriber,
        crate::iceoryx2::BundleMembersAtTheRead::default(),
    );

    publish_one_frame(&publisher, "out", b"queued-0");
    publish_one_frame(&publisher, "out", b"queued-1");
    mailboxes.receive_pending();
    for index in 0..RING_DEPTH + BAGS_THE_RING_OVERWROTE {
        publish_one_frame(&publisher, "out", format!("after-{index}").as_bytes());
    }

    assert_eq!(
        payloads_and_losses_read_off(&mailboxes, "in"),
        [
            ("queued-0", 0),
            ("queued-1", 0),
            ("after-3", BAGS_THE_RING_OVERWROTE as u64),
            ("after-4", 0),
            ("after-5", 0),
            ("after-6", 0),
        ]
        .map(|(payload, lost)| (payload.to_string(), lost)),
    );
}

/// A bag the mailbox evicted is told to the next bag read from its link,
/// though that bag was queued before the eviction was counted.
///
/// Four bags reach a two-deep mailbox in one receive, so the third and the
/// fourth each evict one. Fail-without-fix: tell each bag the link's whole
/// count as it stood when the bag was queued, and the third is told of one
/// loss and the fourth of the other — the fourth was never behind a loss.
#[test]
fn an_evicted_bag_is_told_to_the_next_bag_read_from_its_link() {
    let (publisher, subscriber) = open_channel_for_one_link("loss-placement/eviction", 8);
    let mailboxes = InputMailboxesInner::new();
    mailboxes.add_port("in", 2, ReadMode::ReadNextInOrder);
    mailboxes.add_channel_subscriber(
        "in",
        "L-eviction",
        &InboundLinkName::from("psource/out"),
        subscriber,
        crate::iceoryx2::BundleMembersAtTheRead::default(),
    );

    for index in 0..4 {
        publish_one_frame(&publisher, "out", format!("bag-{index}").as_bytes());
    }

    assert_eq!(
        payloads_and_losses_read_off(&mailboxes, "in"),
        [("bag-2".to_string(), 2), ("bag-3".to_string(), 0)],
    );
    assert_eq!(
        mailboxes
            .dropped_bag_counts_by_inbound_link()
            .dropped_bag_count_snapshot_by_inbound_link()["L-eviction"],
        2,
        "telling the reader zeroes nothing: `graph` still renders the cumulative count"
    );
}

/// Under fan-in each read is told its own link's losses, never the other
/// link's. Each link's first receive is the baseline its ring gaps are read
/// against, so both deliver once before one is overrun.
///
/// Fail-without-fix: keep one reader-side tally per port and the first bag
/// read from the healthy link is told of the overrun link's losses.
#[test]
fn a_fan_in_read_is_told_only_the_losses_of_the_link_it_arrived_on() {
    const RING_DEPTH: usize = 2;
    let (overrun_publisher, overrun_subscriber) =
        open_channel_for_one_link("loss-placement/fan-in/overrun", RING_DEPTH);
    let (healthy_publisher, healthy_subscriber) =
        open_channel_for_one_link("loss-placement/fan-in/healthy", RING_DEPTH);
    let mailboxes = InputMailboxesInner::new();
    mailboxes.add_port("in", 64, ReadMode::ReadNextInOrder);
    mailboxes.add_channel_subscriber(
        "in",
        "L-overrun",
        &InboundLinkName::from("poverrun/out"),
        overrun_subscriber,
        crate::iceoryx2::BundleMembersAtTheRead::default(),
    );
    mailboxes.add_channel_subscriber(
        "in",
        "L-healthy",
        &InboundLinkName::from("phealthy/out"),
        healthy_subscriber,
        crate::iceoryx2::BundleMembersAtTheRead::default(),
    );

    publish_one_frame(&overrun_publisher, "out", b"overrun-first");
    publish_one_frame(&healthy_publisher, "out", b"healthy-first");
    mailboxes.receive_pending();
    for index in 0..RING_DEPTH + 3 {
        publish_one_frame(
            &overrun_publisher,
            "out",
            format!("overrun-{index}").as_bytes(),
        );
    }
    publish_one_frame(&healthy_publisher, "out", b"healthy-second");

    let mut losses_told_by_link: std::collections::BTreeMap<String, Vec<u64>> =
        std::collections::BTreeMap::new();
    while let Some((message, inbound_link_name)) =
        mailboxes.read_raw_from_inbound_link("in").unwrap()
    {
        losses_told_by_link
            .entry(inbound_link_name.as_str().to_string())
            .or_default()
            .push(message.lost_since_last_read);
    }
    assert_eq!(
        losses_told_by_link,
        std::collections::BTreeMap::from([
            ("phealthy/out".to_string(), vec![0, 0]),
            ("poverrun/out".to_string(), vec![0, 3, 0]),
        ]),
    );
}

/// A frame for a link whose local port has no mailbox consumed a sequence
/// number and reaches no reader, so it is counted on its link.
#[test]
fn a_frame_bound_to_a_port_with_no_mailbox_is_counted_on_its_link() {
    let (publisher, subscriber) = open_channel_for_one_link("no-mailbox", 4);
    let mailboxes = InputMailboxesInner::new();
    mailboxes.add_channel_subscriber(
        "in",
        "L-portless",
        &InboundLinkName::from("psource/out"),
        subscriber,
        crate::iceoryx2::BundleMembersAtTheRead::default(),
    );

    publish_one_frame(&publisher, "out", b"nowhere to go");
    publish_one_frame(&publisher, "out", b"nowhere to go either");
    mailboxes.receive_pending();

    assert_eq!(
        dropped_bag_counts_of(&mailboxes),
        std::collections::BTreeMap::from([("L-portless".to_string(), 2)]),
    );
}

/// An `ordered` reader that read a port's last message reads it once, though the port sends
/// it again for a reader that connected after it: the repeat carries the number the reader
/// received last, is passed over and counted taken, and is told as no loss.
///
/// Fail-without-fix: the reader reads the last message twice, as the counter's first reader did
/// when a second reader was linked to it live.
#[test]
fn an_ordered_reader_reads_once_a_message_sent_again_for_a_later_reader() {
    let channel = crate::iceoryx2::Iceoryx2Node::for_this_test_process()
        .open_or_create_service(&unique_suffix("sent-again-for-a-later-reader"), 3, 16)
        .unwrap();
    let mailboxes = InputMailboxesInner::new();
    mailboxes.add_port("in", 16, ReadMode::ReadNextInOrder);
    mailboxes.add_channel_subscriber(
        "in",
        "L-first",
        &InboundLinkName::from("psource/out"),
        channel.create_subscriber(16).unwrap(),
        crate::iceoryx2::BundleMembersAtTheRead::default(),
    );
    let output_writer = output_writer_publishing_onto(&channel, "out");
    output_writer.keep_the_last_message_of_output_ports(["out".to_string()]);
    write_starting_a_group(&output_writer, "one");
    write_starting_a_group(&output_writer, "two");
    assert_eq!(
        payloads_and_losses_read_off(&mailboxes, "in"),
        [("one".to_string(), 0), ("two".to_string(), 0)]
    );

    let later_reader = channel.create_subscriber(16).unwrap();
    output_writer.send_each_kept_message_to_a_reader_it_missed();
    write_starting_a_group(&output_writer, "three");

    assert_eq!(
        payloads_and_losses_read_off(&mailboxes, "in"),
        [("three".to_string(), 0)]
    );
    assert_eq!(
        mailboxes
            .dropped_bag_counts_by_inbound_link()
            .taken_bag_count_snapshot_by_inbound_link()
            .get("L-first")
            .copied(),
        Some(4)
    );
    let mut later_reader_read = Vec::new();
    while let Some(sample) = later_reader.receive().unwrap() {
        later_reader_read.push(sample.user_header().sequence_number);
    }
    assert_eq!(later_reader_read, [1, 2]);
}
