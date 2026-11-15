// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

/// Driving the iceoryx2 Event service end-to-end: notify must transition
/// the Listener fd to readable within a short bounded window so an epoll
/// or poll wait wakes promptly.
#[test]
fn listener_fd_is_valid_and_readable_after_notify() {
    let node = crate::iceoryx2::create_iceoryx2_node_for_this_test_process();
    let name = unique_suffix("notify");

    let svc = node
        .service_builder(&ServiceName::new(&name).unwrap())
        .event()
        .max_notifiers(2)
        .max_listeners(1)
        .open_or_create()
        .unwrap();
    let notifier = svc.notifier_builder().create().unwrap();
    let listener =
        crate::iceoryx2::bind_an_iceoryx2_listener_outside_every_child_process_start(&svc).unwrap();

    let mailboxes = InputMailboxesInner::new();
    mailboxes.set_listener(listener);
    let fd = mailboxes
        .listener_fd()
        .expect("listener_fd should be set after set_listener");
    assert!(fd >= 0, "listener fd should be a valid posix fd, got {fd}");

    // Pre-flight: not readable.
    assert!(!poll_readable(fd, 0));

    notifier.notify().unwrap();

    // Bounded wait: the issue requires the fd to report readable within
    // 50 ms. Using a 50 ms poll matches that contract.
    assert!(
        poll_readable(fd, 50),
        "listener fd should be readable within 50 ms of notify()"
    );

    // After draining, the fd transitions back to not-readable so the
    // next wait blocks again instead of spinning.
    mailboxes.drain_listener();
    assert!(!poll_readable(fd, 0));
}

fn poll_readable(fd: i32, timeout_ms: i32) -> bool {
    let mut pfd = libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: fd is a valid POSIX fd for the lifetime of this call;
    // pfd is on the stack and not aliased.
    let n = unsafe { libc::poll(&mut pfd, 1, timeout_ms) };
    n > 0 && (pfd.revents & libc::POLLIN) != 0
}

/// Regression lock for the reactive-scheduler burst-drain path:
/// `any_port_has_data()` must reflect total queued depth across all
/// configured ports.
#[test]
fn any_port_has_data_reflects_total_queued_depth() {
    let mailboxes = InputMailboxesInner::new();
    mailboxes.add_port("port_a", 64, ReadMode::ReadNextInOrder);
    mailboxes.add_port("port_b", 64, ReadMode::ReadNextInOrder);

    assert!(
        !mailboxes.any_port_has_data(),
        "empty mailboxes report no data"
    );

    let link_into_port_a = a_test_link_into(&mailboxes, "port_a");
    let link_into_port_b = a_test_link_into(&mailboxes, "port_b");
    let frame = TestChannelSample::carrying(0, &[1, 2, 3, 4]);

    // Burst: three frames on port_a, two on port_b.
    for _ in 0..3 {
        link_into_port_a.send_numbered(&frame);
    }
    for _ in 0..2 {
        link_into_port_b.send_numbered(&frame);
    }

    // All 5 are queued; any_port_has_data sees them.
    assert!(
        mailboxes.any_port_has_data(),
        "five queued frames must report has_data"
    );

    // Drain port_a entirely via read_raw (skips msgpack deserialization
    // of the synthetic payload).
    for _ in 0..3 {
        assert!(
            mailboxes
                .read_raw("port_a")
                .expect("read_raw port_a ok")
                .is_some(),
            "port_a should still have a frame",
        );
    }
    assert!(
        mailboxes.any_port_has_data(),
        "port_a empty but port_b still has 2 frames",
    );

    // Drain the other.
    for _ in 0..2 {
        assert!(
            mailboxes
                .read_raw("port_b")
                .expect("read_raw port_b ok")
                .is_some(),
            "port_b should still have a frame",
        );
    }
    assert!(
        !mailboxes.any_port_has_data(),
        "both ports drained — must report no data",
    );
}

/// N→1 fan-in delivery: a destination consuming TWO inbound
/// channels binds two subscribers to ONE local input port; `receive_pending`
/// routes every frame from both channels into that shared mailbox.
///
/// The two source channels stamp DIFFERENT source ports, so the routing must
/// be by the subscriber→local-port binding, not the frame's stamped key.
/// Revert lock: route by the stamped source port instead (as `route()` does)
/// and both frames look for mailboxes named after the source ports — which
/// don't exist on this destination — so the "in" mailbox stays empty and the
/// two-frame assertion fails.
#[test]
fn two_channel_subscribers_fan_into_one_local_port() {
    let (publisher_a, sub_a) = open_channel_for_one_link("fanin/a", 1);
    let (publisher_b, sub_b) = open_channel_for_one_link("fanin/b", 1);
    publish_one_frame(&publisher_a, "src_a_out", b"frame-from-a");
    publish_one_frame(&publisher_b, "src_b_out", b"frame-from-b");

    let mailboxes = InputMailboxesInner::new();
    mailboxes.add_port("in", 64, ReadMode::ReadNextInOrder);
    mailboxes.add_channel_subscriber(
        "in",
        "L-fanin-a",
        &InboundLinkName::from("pfanin-a/out"),
        sub_a,
    );
    mailboxes.add_channel_subscriber(
        "in",
        "L-fanin-b",
        &InboundLinkName::from("pfanin-b/out"),
        sub_b,
    );

    let mut payloads: Vec<Vec<u8>> = Vec::new();
    while let Some(message) = mailboxes.read_raw("in").unwrap() {
        payloads.push(message.payload.to_vec());
    }
    payloads.sort();
    assert_eq!(
        payloads,
        vec![b"frame-from-a".to_vec(), b"frame-from-b".to_vec()],
        "both inbound channels must fan into the one local input port's mailbox",
    );
}

/// The counting contract under fan-in: a stalled `ordered` consumer whose
/// port two links feed reports each link's OWN losses, and the counts sum
/// to published minus delivered — the 78-of-378 audio arrangement, made
/// visible.
///
/// The attribution is what makes this more than a total. Frames arrive
/// A×5 then B×5 into a depth-2 mailbox, so B's arrivals evict A's bags:
/// charging the pushing link would report A=3/B=5, charging the link the
/// evicted bag came in on reports A=5/B=3. The tag rides the entry for
/// exactly this reason.
///
/// The stall here is the consumer never reading — the transport is pumped
/// after every publish on purpose, so what the counts account for is
/// eviction at the mailbox alone. A consumer parked deeper, pumping no
/// receive at all, overflows the iceoryx2 subscriber ring instead, which
/// the sequence-number gap counts on the same link.
#[test]
fn each_inbound_link_reports_its_own_losses_at_a_stalled_ordered_port() {
    const MAILBOX_DEPTH: usize = 2;
    const FRAMES_PER_LINK: usize = 5;

    let (publisher_a, subscriber_a) = open_channel_for_one_link("drop-count/a", FRAMES_PER_LINK);
    let (publisher_b, subscriber_b) = open_channel_for_one_link("drop-count/b", FRAMES_PER_LINK);

    let mailboxes = InputMailboxesInner::new();
    mailboxes.add_port("in", MAILBOX_DEPTH, ReadMode::ReadNextInOrder);
    mailboxes.add_channel_subscriber(
        "in",
        "L-first",
        &InboundLinkName::from("pfirst/out"),
        subscriber_a,
    );
    mailboxes.add_channel_subscriber(
        "in",
        "L-second",
        &InboundLinkName::from("psecond/out"),
        subscriber_b,
    );

    for _ in 0..FRAMES_PER_LINK {
        publish_one_frame(&publisher_a, "src_a_out", b"from-a");
        mailboxes.receive_pending();
    }
    for _ in 0..FRAMES_PER_LINK {
        publish_one_frame(&publisher_b, "src_b_out", b"from-b");
        mailboxes.receive_pending();
    }

    let counts = mailboxes
        .dropped_bag_counts_by_inbound_link()
        .dropped_bag_count_snapshot_by_inbound_link();
    assert_eq!(
        counts,
        std::collections::BTreeMap::from(
            [("L-first".to_string(), 5), ("L-second".to_string(), 3),]
        ),
        "each link must carry the bags IT lost, not the ones it displaced",
    );

    let delivered = mailboxes.drain("in").len();
    assert_eq!(delivered, MAILBOX_DEPTH);
    assert_eq!(
        counts.values().sum::<u64>() + delivered as u64,
        (FRAMES_PER_LINK * 2) as u64,
        "counted plus delivered must account for every bag published",
    );
}

/// A helper's losses reach its board as they are counted, with nothing
/// pumping them there: a mailbox eviction lands on the evicted bag's link's
/// slot, and a windowed port's flush on its one link's slot.
///
/// Fail-without-fix: mirror nothing, or only on a pass the helper's loop
/// makes, and both slots still read zero when the loss has already
/// happened.
#[test]
fn every_loss_counted_on_a_mirrored_link_reaches_its_board_slot_as_it_is_counted() {
    let (board, board_writer, _helper_node) =
        crate::iceoryx2::a_loss_count_board_and_its_helpers_writer_for_this_test_process(&[], &[]);
    let mailboxes = InputMailboxesInner::new();

    let (plain_publisher, plain_subscriber) = open_channel_for_one_link("mirrored/plain", 5);
    mailboxes.add_port("in", 2, ReadMode::ReadNextInOrder);
    mailboxes.add_channel_subscriber(
        "in",
        "L-plain",
        &InboundLinkName::from("pplain/out"),
        plain_subscriber,
    );
    mailboxes
        .mirror_an_inbound_links_loss_counts_into(
            "L-plain",
            board_writer
                .claim_inbound_link_slot(
                    crate::iceoryx2::InboundLinkLossCountBoardSlotAndWiringGeneration {
                        slot: 0,
                        wiring_generation: 11,
                    },
                )
                .unwrap(),
        )
        .expect("a bound link's counts mirror");

    let (windowed_publisher, windowed_subscriber) =
        open_channel_for_one_link("mirrored/windowed", 4);
    mailboxes.add_windowed_port(
        "audio",
        ReadMode::ReadNextInOrder,
        a_512_512_contract_at(16_000, 1),
    );
    mailboxes.add_channel_subscriber(
        "audio",
        "L-windowed",
        &InboundLinkName::from("pwindowed/out"),
        windowed_subscriber,
    );
    mailboxes
        .mirror_an_inbound_links_loss_counts_into(
            "L-windowed",
            board_writer
                .claim_inbound_link_slot(
                    crate::iceoryx2::InboundLinkLossCountBoardSlotAndWiringGeneration {
                        slot: 1,
                        wiring_generation: 12,
                    },
                )
                .unwrap(),
        )
        .expect("a windowed link's counts mirror");

    for _ in 0..5 {
        publish_one_frame(&plain_publisher, "src_out", b"bag");
        mailboxes.receive_pending();
    }
    assert_eq!(
        board.inbound_link_slot(0),
        Some(streamlib_ipc_types::InboundLinkLossCountBoardSlot {
            wiring_generation: 11,
            dropped_bags: 3,
            ..Default::default()
        }),
    );
    let _ = mailboxes.read_raw("in").unwrap();
    assert_eq!(
        board
            .inbound_link_slot(0)
            .map(|slot| slot.bags_off_the_mailbox_for_its_reader),
        Some(1),
        "a bag read reaches the slot as taken as it is read"
    );

    publish_one_mono_channels_block(&windowed_publisher, 300, 16_000, 0);
    assert!(mailboxes.read_raw("audio").unwrap().is_none());
    publish_one_mono_channels_block(&windowed_publisher, 300, 16_000, 1_000_000_000);
    let _ = mailboxes.read_raw("audio").unwrap();
    assert_eq!(
        board.inbound_link_slot(1).map(|slot| (
            slot.wiring_generation,
            slot.dropped_bags,
            slot.discarded_samples
        )),
        Some((12, 0, 300)),
    );

    assert!(
        mailboxes
            .mirror_an_inbound_links_loss_counts_into(
                "L-plain",
                board_writer
                    .claim_inbound_link_slot(
                        crate::iceoryx2::InboundLinkLossCountBoardSlotAndWiringGeneration {
                            slot: 2,
                            wiring_generation: 13,
                        }
                    )
                    .unwrap(),
            )
            .is_err(),
        "a link mirrors onto one slot"
    );
    assert!(
        mailboxes
            .mirror_an_inbound_links_loss_counts_into(
                "L-never-wired",
                board_writer
                    .claim_inbound_link_slot(
                        crate::iceoryx2::InboundLinkLossCountBoardSlotAndWiringGeneration {
                            slot: 3,
                            wiring_generation: 14,
                        }
                    )
                    .unwrap(),
            )
            .is_err(),
        "a link with no subscriber has no counts to mirror"
    );
}

/// A publisher that replaced the one a live subscriber was hearing from
/// numbers its sends from zero again, and the first of them the subscriber
/// receives is a new baseline, never a gap measured against the old one.
///
/// The replacement's first two bags are overwritten in a four-deep ring, so
/// its first received number is two past the old publisher's last.
/// Fail-without-fix: compare numbers without the publisher that sent them
/// and the link reports one lost bag the replacement never counted as sent
/// to it.
#[test]
fn a_replacement_publisher_on_a_live_subscriber_reads_as_a_new_baseline_not_a_gap() {
    const RING_DEPTH: usize = 4;
    let channel = crate::iceoryx2::Iceoryx2Node::for_this_test_process()
        .open_or_create_service(&unique_suffix("replaced-publisher"), 2, 16)
        .unwrap();
    let mailboxes = InputMailboxesInner::new();
    mailboxes.add_port("in", 16, ReadMode::ReadNextInOrder);
    mailboxes.add_channel_subscriber(
        "in",
        "L-replaced",
        &InboundLinkName::from("psource/out"),
        channel.create_subscriber(RING_DEPTH).unwrap(),
    );

    let output_writer = output_writer_publishing_onto(&channel, "out");
    output_writer
        .write_bytes_with_timestamp("out", b"old", 0, MessageGroupBoundary::StartsGroup)
        .unwrap();
    mailboxes.receive_pending();

    assert!(output_writer.remove_channel_link("out", "L-to-the-destination"));
    install_a_fresh_publisher(&output_writer, &channel, "out");
    for bag in 0..RING_DEPTH + 2 {
        output_writer
            .write_bytes_with_timestamp(
                "out",
                b"new",
                bag as i64,
                MessageGroupBoundary::StartsGroup,
            )
            .unwrap();
    }
    mailboxes.receive_pending();

    assert_eq!(
        dropped_bag_counts_of(&mailboxes),
        std::collections::BTreeMap::from([("L-replaced".to_string(), 0)]),
        "the replacement's first received bag is a baseline"
    );
    assert_eq!(mailboxes.drain("in").len(), 1 + RING_DEPTH);
}
