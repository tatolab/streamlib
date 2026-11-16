// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

/// A disconnected link's count leaves with it: `graph` renders counts
/// beside links it still has, and a count naming a link the graph dropped
/// is a reader's dead end.
#[test]
fn a_disconnected_links_count_goes_with_the_link() {
    let (publisher, subscriber) = open_channel_for_one_link("reclaim-count", 4);

    let mailboxes = InputMailboxesInner::new();
    mailboxes.add_port("in", 1, ReadMode::ReadNextInOrder);
    mailboxes.add_channel_subscriber(
        "in",
        "L-departing",
        &InboundLinkName::from("pdeparting/out"),
        subscriber,
    );
    for _ in 0..3 {
        publish_one_frame(&publisher, "src_out", b"body");
        mailboxes.receive_pending();
    }
    assert_eq!(
        mailboxes
            .dropped_bag_counts_by_inbound_link()
            .dropped_bag_count_snapshot_by_inbound_link()["L-departing"],
        2,
    );

    mailboxes.remove_channel_link("L-departing");

    assert!(
        mailboxes
            .dropped_bag_counts_by_inbound_link()
            .dropped_bag_count_snapshot_by_inbound_link()
            .is_empty(),
    );
}

/// Per-link destination reclaim: a destination fanning two inbound
/// links into ONE local port holds two tagged subscribers plus one shared
/// listener. Disconnecting one link drops only its subscriber (the port
/// mailbox and listener survive so the other link keeps delivering);
/// disconnecting the last link removes the port mailbox AND drops the shared
/// listener — releasing the notify service so a reconnect recreates it fresh.
///
/// Fail-without-fix: make `remove_channel_link` a no-op and the final
/// disconnect leaves the port and listener live, so `has_port` /
/// `has_listener` stay true and the release assertions fail.
#[test]
fn remove_channel_link_reclaims_per_link_then_drops_port_and_listener() {
    let node = crate::iceoryx2::create_iceoryx2_node_for_this_test_process();

    let open_subscriber = |tag: &str| {
        crate::iceoryx2::Iceoryx2Node::for_this_test_process()
            .open_or_create_service(&unique_suffix(tag), 2, 4)
            .unwrap()
            .create_subscriber(4)
            .unwrap()
    };
    let listener = crate::iceoryx2::bind_an_iceoryx2_listener_outside_every_child_process_start(
        &node
            .service_builder(&ServiceName::new(&unique_suffix("reclaim/notify")).unwrap())
            .event()
            .max_notifiers(2)
            .max_listeners(1)
            .open_or_create()
            .unwrap(),
    )
    .unwrap();

    let inner = InputMailboxesInner::new();
    inner.add_port("in", 64, ReadMode::ReadNextInOrder);
    inner.add_channel_subscriber(
        "in",
        "L-link-a",
        &InboundLinkName::from("plink-a/out"),
        open_subscriber("reclaim/a"),
    );
    inner.add_channel_subscriber(
        "in",
        "L-link-b",
        &InboundLinkName::from("plink-b/out"),
        open_subscriber("reclaim/b"),
    );
    inner.set_listener(listener);
    assert!(inner.has_port("in"));
    assert!(inner.has_listener());

    // Disconnect one of two links into the shared port: port + listener stay.
    inner.remove_channel_link("L-link-a");
    assert!(
        inner.has_port("in"),
        "the local port must stay while link-b still feeds it",
    );
    assert!(
        inner.has_listener(),
        "the shared listener must stay while any inbound link remains",
    );

    // Unknown link id is a no-op.
    inner.remove_channel_link("L-does-not-exist");
    assert!(inner.has_port("in"));

    // Disconnect the last link: port mailbox removed, listener released.
    inner.remove_channel_link("L-link-b");
    assert!(
        !inner.has_port("in"),
        "the port mailbox must be reclaimed once its last subscriber is gone",
    );
    assert!(
        !inner.has_listener(),
        "the destination's listener (and its notify service) must be released \
         after the last inbound link disconnects so a reconnect recreates it",
    );
}

/// Empty (unwired) mailboxes should return Ok(None) from read_raw
/// rather than crash. Mentally revert the is_configured guard
/// and the test panics dereferencing a null handle.
#[test]
fn empty_mailboxes_returns_none_cleanly() {
    let mb = InputMailboxes::empty();
    assert!(!mb.is_configured());
    assert!(mb.read_raw("any").unwrap().is_none());
    assert!(!mb.has_data("any"));
}

/// The payload length is the last wire-derived number the read path trusts,
/// and a frame claiming more than it carries has no safe default — it is
/// unusable, so it must surface as a typed error naming the port and both
/// numbers rather than slicing past the frame.
///
/// Fail-without-fix: drop the bound and the read slices `[..4096]` out of
/// a 64-byte sample.
#[test]
fn a_read_rejects_a_frame_stamping_more_payload_than_it_carries() {
    const STAMPED: u32 = 4096;
    const CARRIED: usize = 64;

    let inner = InputMailboxesInner::new();
    inner.add_port("in", 64, ReadMode::ReadNextInOrder);
    let link = a_test_link_into(&inner, "in");

    link.send_numbered(&TestChannelSample::stamping(42, STAMPED, &[0u8; CARRIED]));

    let err = match inner.read_raw("in") {
        Err(e) => e,
        Ok(_) => panic!("a frame stamping more than it carries must not read"),
    };
    assert!(
        matches!(
            &err,
            Error::ChannelSampleStampsAPayloadPastTheSample {
                port,
                stamped_payload_bytes,
                available_payload_bytes,
            } if port == "in"
                && *stamped_payload_bytes == STAMPED as usize
                && *available_payload_bytes == CARRIED
        ),
        "expected the typed length error naming the port and both numbers, got {err:?}"
    );
    // Diagnosable without a debugger: the rendered message carries all three.
    let rendered = err.to_string();
    for expected in ["'in'", "4096", "64"] {
        assert!(
            rendered.contains(expected),
            "message must name {expected}: {rendered}"
        );
    }

    // The malformed frame is dropped — the port keeps serving.
    let body = [1u8, 2, 3, 4];
    link.send_numbered(&TestChannelSample::carrying(43, &body));

    let MessageReadFromInputPort {
        payload: data,
        timestamp_ns,
        ..
    } = inner
        .read_raw("in")
        .expect("read")
        .expect("the well-formed frame delivers");
    assert_eq!(data, body, "a well-formed frame still delivers intact");
    assert_eq!(timestamp_ns, 43);
}

/// A frame may carry more bytes than its header stamps — the wire
/// contract trusts the stamped length, so the read delivers exactly that
/// many payload bytes and the slack past them never reaches the caller.
///
/// Fail-without-fix: hand the caller the frame's whole tail instead of
/// slicing (or truncating) to the stamped length and the delivered
/// payload grows by the slack bytes — the exact-length assertion fails.
#[test]
fn a_read_delivers_exactly_the_stamped_payload_from_an_over_carrying_frame() {
    const STAMPED: usize = 200;
    const CARRIED: usize = 300;

    let inner = InputMailboxesInner::new();
    inner.add_port("in", 8, ReadMode::ReadNextInOrder);
    let link = a_test_link_into(&inner, "in");

    let body: Vec<u8> = (0..CARRIED as u32).map(|i| (i % 251) as u8).collect();
    link.send_numbered(&TestChannelSample::stamping(77, STAMPED as u32, &body));

    let MessageReadFromInputPort {
        payload: data,
        timestamp_ns,
        ..
    } = inner
        .read_raw("in")
        .expect("read")
        .expect("the over-carrying frame delivers its stamped payload");
    assert_eq!(
        data.len(),
        STAMPED,
        "payload must stop at the stamped length"
    );
    assert_eq!(
        data[..],
        body[..STAMPED],
        "payload must be the stamped prefix"
    );
    assert_eq!(timestamp_ns, 77);

    // The slack was dropped with the frame — the port is empty.
    assert!(inner.read_raw("in").expect("read").is_none());
}

/// Clone bumps the strong count via the host-installed
/// refcount fn; both clones drop independently.
#[test]
fn clone_balances_drop() {
    let inner = Arc::new(InputMailboxesInner::new());
    let inner_for_test = inner.clone();
    let mb1 = InputMailboxes::from_inner_arc(inner);
    assert_eq!(Arc::strong_count(&inner_for_test), 2);
    let mb2 = mb1.clone();
    assert_eq!(Arc::strong_count(&inner_for_test), 3);
    drop(mb2);
    assert_eq!(Arc::strong_count(&inner_for_test), 2);
    drop(mb1);
    assert_eq!(Arc::strong_count(&inner_for_test), 1);
}

#[test]
fn a_link_wired_and_unwired_in_a_loop_never_races_a_read_that_holds_no_node_mutex() {
    const RACE_WINDOW: std::time::Duration = std::time::Duration::from_secs(3);
    const RACE_HARD_CAP: std::time::Duration = std::time::Duration::from_secs(30);
    const WIRE_AND_UNWIRE_CYCLES_FLOOR: u64 = 100;
    const READ_PASSES_FLOOR: u64 = 100;
    const MAX_SUBSCRIBERS: usize = 2;
    const MAX_QUEUED_MESSAGES: usize = 16;
    const MAX_NOTIFIERS: usize = 1;
    const EXPECTED_PAYLOAD_BYTES: usize = 64;
    const RACERS: usize = 2;

    /// Counts a racer out when it ends, by returning or by panicking, so the
    /// publisher it feeds never outlives it.
    struct RacerStillRunning<'a>(&'a std::sync::atomic::AtomicUsize);
    impl Drop for RacerStillRunning<'_> {
        fn drop(&mut self) {
            self.0.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
        }
    }

    let node = crate::iceoryx2::Iceoryx2Node::for_this_test_process();
    let channel_service_name = unique_suffix("live-rewire-race/channel");
    let notify_service_name = unique_suffix("live-rewire-race/notify");
    let inbound_link_name = InboundLinkName::from(channel_service_name.as_str());
    let open_the_rewired_channel = || {
        node.open_or_create_service(&channel_service_name, MAX_SUBSCRIBERS, MAX_QUEUED_MESSAGES)
            .unwrap()
    };
    let mailboxes = InputMailboxesInner::new();
    let racers_still_running = std::sync::atomic::AtomicUsize::new(RACERS);
    let wiring_floor_met = std::sync::atomic::AtomicBool::new(false);
    let reading_floor_met = std::sync::atomic::AtomicBool::new(false);
    let race_started = std::time::Instant::now();
    let still_racing = || {
        let elapsed = race_started.elapsed();
        let both_floors_met = wiring_floor_met.load(std::sync::atomic::Ordering::Relaxed)
            && reading_floor_met.load(std::sync::atomic::Ordering::Relaxed);
        elapsed < RACE_HARD_CAP
            && racers_still_running.load(std::sync::atomic::Ordering::Relaxed) == RACERS
            && (elapsed < RACE_WINDOW || !both_floors_met)
    };

    let (wire_and_unwire_cycles, read_passes, frames_read) = std::thread::scope(|scope| {
        scope.spawn(|| {
            let publisher = TestChannelPublisherNumberingItsSends::numbering(
                open_the_rewired_channel()
                    .create_publisher(EXPECTED_PAYLOAD_BYTES)
                    .unwrap(),
            );
            while racers_still_running.load(std::sync::atomic::Ordering::Relaxed) > 0
                && race_started.elapsed() < RACE_HARD_CAP
            {
                publish_one_frame(&publisher, "out", b"a-bag-published-mid-rewire");
            }
        });

        let wiring = scope.spawn(|| {
            let _still_running = RacerStillRunning(&racers_still_running);
            let mut wire_and_unwire_cycles = 0u64;
            while still_racing() {
                if !mailboxes.has_port("in") {
                    mailboxes.add_port("in", MAX_QUEUED_MESSAGES, ReadMode::ReadNextInOrder);
                }
                mailboxes.add_channel_subscriber(
                    "in",
                    "L-rewired",
                    &inbound_link_name,
                    open_the_rewired_channel()
                        .create_subscriber(MAX_QUEUED_MESSAGES)
                        .unwrap(),
                );
                if !mailboxes.has_listener() {
                    mailboxes.set_listener(
                        node.open_or_create_notify_service(&notify_service_name, MAX_NOTIFIERS)
                            .unwrap()
                            .create_listener()
                            .unwrap(),
                    );
                }
                mailboxes.remove_channel_link("L-rewired");
                wire_and_unwire_cycles += 1;
                if wire_and_unwire_cycles >= WIRE_AND_UNWIRE_CYCLES_FLOOR {
                    wiring_floor_met.store(true, std::sync::atomic::Ordering::Relaxed);
                }
            }
            wire_and_unwire_cycles
        });

        let reading = scope.spawn(|| {
            let _still_running = RacerStillRunning(&racers_still_running);
            let mut read_passes = 0u64;
            let mut frames_read = 0u64;
            while still_racing() {
                mailboxes.drain_listener();
                let _ = mailboxes.listener_fd();
                let _ = mailboxes.inbound_link_names("in");
                if mailboxes.has_data("in") {
                    while let Ok(Some(_)) = mailboxes.read_raw("in") {
                        frames_read += 1;
                    }
                }
                let _ = mailboxes.any_port_has_data();
                read_passes += 1;
                if read_passes >= READ_PASSES_FLOOR && frames_read > 0 {
                    reading_floor_met.store(true, std::sync::atomic::Ordering::Relaxed);
                }
            }
            (read_passes, frames_read)
        });

        let wire_and_unwire_cycles = wiring.join().unwrap();
        let (read_passes, frames_read) = reading.join().unwrap();
        (wire_and_unwire_cycles, read_passes, frames_read)
    });

    assert!(
        wire_and_unwire_cycles >= WIRE_AND_UNWIRE_CYCLES_FLOOR,
        "the wiring thread must re-splice the link many times before the hard cap, \
         got {wire_and_unwire_cycles}"
    );
    assert!(
        read_passes >= READ_PASSES_FLOOR,
        "the reading thread must read many times before the hard cap, got {read_passes}"
    );
    assert!(
        frames_read > 0,
        "reads must have interleaved with a wired link, but none returned a bag \
         across {read_passes} passes and {wire_and_unwire_cycles} re-splices"
    );
}
