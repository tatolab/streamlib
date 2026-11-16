// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

/// A loan is filled in place and published as one sample carrying its
/// stamp; a loan dropped unpublished sends nothing.
#[test]
fn a_loan_is_published_in_place_and_a_dropped_loan_sends_nothing() {
    let pubsub = open_channel_data_service("loan/publish", 2);
    let subscriber = pubsub.create_subscriber(4).unwrap();
    let inner = output_writer_with_one_channel(
        &pubsub,
        crate::iceoryx2::TRUSTED_CHANNEL_CHUNK_CEILING_BYTES,
    );

    let mut loan = inner.loan_output_port_payload("out", 5).unwrap();
    assert_eq!(loan.payload_bytes_mut().len(), 5);
    assert_eq!(loan.payload_bytes_mut().as_ptr() as usize % 64, 0);
    loan.payload_bytes_mut().copy_from_slice(b"hello");
    inner
        .publish_output_port_payload_loan(
            loan,
            99,
            MessageGroupBoundary::ContinuesGroup,
            DataChannelPayloadDescriptor::bytes(),
        )
        .unwrap();
    let sample = subscriber.receive().unwrap().expect("the published loan");
    assert_eq!(stamped_payload_of(&sample), b"hello");
    assert_eq!(sample.user_header().timestamp_ns, 99);
    assert_eq!(
        sample.user_header().group_boundary(),
        MessageGroupBoundary::ContinuesGroup
    );
    assert_eq!(written_bag_count_of(&inner, "out"), 1);

    let mut given_back = inner.loan_output_port_payload("out", 3).unwrap();
    given_back.payload_bytes_mut().copy_from_slice(b"bye");
    drop(given_back);
    assert!(subscriber.receive().unwrap().is_none());
    assert_eq!(written_bag_count_of(&inner, "out"), 1);

    inner
        .write_bytes_with_timestamp("out", b"after", 1, MessageGroupBoundary::StartsGroup)
        .expect("a given-back loan leaves the port free to write");
    assert_eq!(
        stamped_payload_of(&subscriber.receive().unwrap().unwrap()),
        b"after"
    );
}

/// While a loan of a port is open, a second loan and a write on it are
/// each refused naming the open loan, and the loan still publishes.
#[test]
fn an_open_loan_refuses_a_second_loan_and_a_write_on_its_port() {
    let pubsub = open_channel_data_service("loan/refusals", 2);
    let subscriber = pubsub.create_subscriber(4).unwrap();
    let inner = output_writer_with_one_channel(
        &pubsub,
        crate::iceoryx2::TRUSTED_CHANNEL_CHUNK_CEILING_BYTES,
    );

    let mut loan = inner.loan_output_port_payload("out", 1).unwrap();
    let second_loan = inner
        .loan_output_port_payload("out", 1)
        .unwrap_err()
        .to_string();
    assert!(
        second_loan.contains("'out'") && second_loan.contains("loan open"),
        "{second_loan}"
    );
    let write = inner
        .write_bytes_with_timestamp("out", b"x", 0, MessageGroupBoundary::StartsGroup)
        .unwrap_err()
        .to_string();
    assert!(
        write.contains("'out'") && write.contains("loan open"),
        "{write}"
    );

    loan.payload_bytes_mut()[0] = 7;
    inner
        .publish_output_port_payload_loan(
            loan,
            0,
            MessageGroupBoundary::StartsGroup,
            DataChannelPayloadDescriptor::bytes(),
        )
        .unwrap();
    assert_eq!(
        stamped_payload_of(&subscriber.receive().unwrap().unwrap()),
        [7]
    );
}

/// A loan on a declared port no link has reached hands out scratch bytes,
/// and publishing them sends nothing, as a write to such a port does.
#[test]
fn a_loan_on_an_unlinked_declared_port_publishes_nothing() {
    let inner = OutputWriterInner::new();
    inner.declare_output_ports(["out".to_string()]);
    let mut loan = inner.loan_output_port_payload("out", 4).unwrap();
    loan.payload_bytes_mut().copy_from_slice(b"none");
    inner
        .publish_output_port_payload_loan(
            loan,
            0,
            MessageGroupBoundary::StartsGroup,
            DataChannelPayloadDescriptor::bytes(),
        )
        .unwrap();
    assert!(inner.loan_output_port_payload("undeclared", 4).is_err());
}

/// A loan taken before its port was unwired and wired again is given back
/// rather than sent on the new publisher's numbering.
#[test]
fn a_loan_outliving_its_ports_publisher_is_given_back() {
    let first = open_channel_data_service("loan/rewired-first", 2);
    let inner = output_writer_with_one_channel(
        &first,
        crate::iceoryx2::TRUSTED_CHANNEL_CHUNK_CEILING_BYTES,
    );
    let loan = inner.loan_output_port_payload("out", 1).unwrap();

    let second = open_channel_data_service("loan/rewired-second", 2);
    let second_subscriber = second.create_subscriber(4).unwrap();
    inner.set_channel_publisher(
        "out",
        &second,
        second.create_publisher(64).unwrap(),
        ChannelEgressConfig {
            service_name: "test/out".to_string(),
            trust_tier: ChannelTrustTier::Trusted,
            expected_payload_bytes: 64,
            chunk_ceiling_bytes: crate::iceoryx2::TRUSTED_CHANNEL_CHUNK_CEILING_BYTES,
        },
    );
    inner
        .publish_output_port_payload_loan(
            loan,
            0,
            MessageGroupBoundary::StartsGroup,
            DataChannelPayloadDescriptor::bytes(),
        )
        .unwrap();
    assert!(second_subscriber.receive().unwrap().is_none());
    assert_eq!(written_bag_count_of(&inner, "out"), 0);
}

/// An empty payload is a real message: it loans no cache line and reads
/// back empty.
#[test]
fn an_empty_payload_is_written_and_read_back_empty() {
    let pubsub = open_channel_data_service("loan/empty", 2);
    let subscriber = pubsub.create_subscriber(4).unwrap();
    let inner = output_writer_with_one_channel(
        &pubsub,
        crate::iceoryx2::TRUSTED_CHANNEL_CHUNK_CEILING_BYTES,
    );
    inner
        .write_bytes_with_timestamp("out", b"", 5, MessageGroupBoundary::StartsGroup)
        .unwrap();
    let sample = subscriber.receive().unwrap().expect("the empty message");
    assert_eq!(sample.payload().len(), 0);
    assert_eq!(stamped_payload_of(&sample), b"");
}

#[test]
fn write_bytes_with_timestamp_calls_notifier() {
    let node = crate::iceoryx2::create_iceoryx2_node_for_this_test_process();
    let notify_name = unique_suffix("notify");

    let pubsub = open_channel_data_service("pubsub", 2);
    let publisher = pubsub.create_publisher(4096).unwrap();
    let _subscriber = pubsub.create_subscriber(4).unwrap();

    let notify = node
        .service_builder(&ServiceName::new(&notify_name).unwrap())
        .event()
        .max_notifiers(2)
        .max_listeners(1)
        .open_or_create()
        .unwrap();
    let notifier = notify.notifier_builder().create().unwrap();
    let listener =
        crate::iceoryx2::bind_an_iceoryx2_listener_outside_every_child_process_start(&notify)
            .unwrap();

    let inner = Arc::new(OutputWriterInner::new());
    inner.set_channel_publisher(
        "out",
        &pubsub,
        publisher,
        ChannelEgressConfig {
            service_name: "test/out".to_string(),
            trust_tier: crate::iceoryx2::ChannelTrustTier::Trusted,
            expected_payload_bytes: 4096,
            chunk_ceiling_bytes: crate::iceoryx2::TRUSTED_CHANNEL_CHUNK_CEILING_BYTES,
        },
    );
    inner.add_channel_link("out", "L-test-notify", notifier);

    // Pre-flight: the listener has no events queued.
    let mut count: usize = 0;
    listener.try_wait(|_| count += 1).unwrap();
    assert_eq!(count, 0);

    let writer = OutputWriter::from_inner_arc(inner);
    writer
        .write_bytes_with_timestamp("out", b"payload", 1234, MessageGroupBoundary::StartsGroup)
        .unwrap();
    writer
        .write_bytes_with_timestamp("out", b"more", 5678, MessageGroupBoundary::StartsGroup)
        .unwrap();

    // Notifier::notify is non-blocking; give iceoryx2 a moment to deliver
    // before draining. timed_wait returns as soon as the first event
    // arrives, so the deadline is generous, not the typical wait time.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
    while count == 0 && std::time::Instant::now() < deadline {
        listener
            .timed_wait(|_| count += 1, std::time::Duration::from_millis(50))
            .unwrap();
    }
    // Drain anything still pending.
    listener.try_wait(|_| count += 1).unwrap();
    assert!(
        count >= 1,
        "expected at least one notify after write_bytes_with_timestamp, got {}",
        count
    );
}

/// A notifier aimed at a destination that never drains its listener is
/// delivered to on every send, and draining the listener later hands back
/// every notification it counted — the iceoryx2 guarantee that lets every
/// destination be notified whatever mode it runs in.
///
/// The observable is `notify()`'s `Ok(usize)` — the number of listeners it
/// actually triggered. A failed delivery does not surface as `Err`: it
/// returns a count that excludes the listener it could not reach. On an
/// event transport that queues a wakeup per send, the undrained listener
/// stops counting after a bounded number of sends (about 280).
#[test]
fn an_undrained_listener_is_delivered_every_send_and_drains_the_whole_count() {
    const SENDS: usize = 8192;

    let node = crate::iceoryx2::create_iceoryx2_node_for_this_test_process();
    let notify_service = node
        .service_builder(&ServiceName::new(&unique_suffix("undrained")).unwrap())
        .event()
        .max_notifiers(1)
        .max_listeners(1)
        .open_or_create()
        .unwrap();
    let notifier = notify_service.notifier_builder().create().unwrap();
    let listener = crate::iceoryx2::bind_an_iceoryx2_listener_outside_every_child_process_start(
        &notify_service,
    )
    .unwrap();

    for send in 0..SENDS {
        assert_eq!(
            notifier.notify().unwrap(),
            1,
            "send {send} reached no listener while nothing drained it"
        );
    }

    let mut drained_notifications = 0;
    listener
        .try_wait(|event_activation| drained_notifications += event_activation.count)
        .unwrap();
    assert_eq!(drained_notifications, SENDS as u64);

    notifier.notify().unwrap();
    let mut notifications_after_the_drain = 0;
    listener
        .try_wait(|event_activation| notifications_after_the_drain += event_activation.count)
        .unwrap();
    assert_eq!(
        notifications_after_the_drain, 1,
        "a drained listener must be woken by the next send"
    );
}

#[test]
fn a_write_naming_no_stamp_is_stamped_with_its_streams_time() {
    let pubsub = open_channel_data_service("stream-clock-stamp", 2);
    let subscriber = pubsub.create_subscriber(4).unwrap();
    let inner = Arc::new(output_writer_with_one_channel(
        &pubsub,
        crate::iceoryx2::TRUSTED_CHANNEL_CHUNK_CEILING_BYTES,
    ));
    let one_second_ago_ns = MediaClock::now().as_nanos() as i64 - 1_000_000_000;
    let half_speed_since_a_second_ago = Arc::new(StreamClock::starting_at(
        crate::core::context::StreamClockSpec::Scaled {
            scale: 0.5,
            step_ns: 1_000,
        },
        one_second_ago_ns,
    ));
    inner.reach_the_stream_clock(Arc::clone(&half_speed_since_a_second_ago));
    let writer = OutputWriter::from_inner_arc(inner);

    let machine_before_ns = MediaClock::now().as_nanos() as i64;
    writer.write_msgpack("out", &42u32).unwrap();
    let machine_after_ns = MediaClock::now().as_nanos() as i64;

    let sample = subscriber
        .receive()
        .unwrap()
        .expect("the write was published");
    let stamp_ns = sample.user_header().timestamp_ns;
    assert!(
        half_speed_since_a_second_ago.stream_time_ns_at(machine_before_ns) <= stamp_ns
            && stamp_ns <= half_speed_since_a_second_ago.stream_time_ns_at(machine_after_ns),
        "{stamp_ns} is not the stream's time across the write"
    );
    assert!(
        stamp_ns < machine_before_ns - 400_000_000,
        "a half-speed clock started a second ago is half a second behind the machine clock"
    );
}

/// 1→N fan-out delivery: a single `write_bytes_with_timestamp` publishes ONE
/// frame that reaches EVERY subscriber on the channel through one zero-copy
/// loan + one send. Three subscribers each receive exactly one copy of the
/// payload.
///
/// Revert lock: change `write_bytes_with_timestamp` to a per-connection copy loop (loan +
/// send once per destination notifier) and each subscriber on the shared
/// service receives N frames instead of one — the exactly-one assertion
/// fails. The capacity-only tests (subscriber-count sizing) stay green under
/// that revert; this is what pins the delivery guarantee.
#[test]
fn write_bytes_with_timestamp_fans_out_single_loan_to_all_subscribers() {
    const N: usize = 3;
    let node = crate::iceoryx2::create_iceoryx2_node_for_this_test_process();

    let pubsub = open_channel_data_service("fanout/pubsub", N + 1);
    let publisher = pubsub.create_publisher(4096).unwrap();
    let subscribers: Vec<_> = (0..N)
        .map(|_| pubsub.create_subscriber(4).unwrap())
        .collect();

    let inner = Arc::new(OutputWriterInner::new());
    inner.set_channel_publisher(
        "out",
        &pubsub,
        publisher,
        ChannelEgressConfig {
            service_name: "test/out".to_string(),
            trust_tier: crate::iceoryx2::ChannelTrustTier::Trusted,
            expected_payload_bytes: 4096,
            chunk_ceiling_bytes: crate::iceoryx2::TRUSTED_CHANNEL_CHUNK_CEILING_BYTES,
        },
    );

    // N destination notifiers on the one channel — the compiler op's wiring
    // shape for N destinations. `write_bytes_with_timestamp` signals each, but the frame is
    // published exactly ONCE.
    let mut listeners = Vec::with_capacity(N);
    for i in 0..N {
        let notify = node
            .service_builder(
                &ServiceName::new(&unique_suffix(&format!("fanout/notify/{i}"))).unwrap(),
            )
            .event()
            .max_notifiers(2)
            .max_listeners(1)
            .open_or_create()
            .unwrap();
        inner.add_channel_link(
            "out",
            &format!("L-test-fanout-{i}"),
            notify.notifier_builder().create().unwrap(),
        );
        listeners.push(
            crate::iceoryx2::bind_an_iceoryx2_listener_outside_every_child_process_start(&notify)
                .unwrap(),
        );
    }

    let writer = OutputWriter::from_inner_arc(inner);
    writer
        .write_bytes_with_timestamp(
            "out",
            b"fanout-payload",
            4242,
            MessageGroupBoundary::StartsGroup,
        )
        .unwrap();

    for (i, subscriber) in subscribers.iter().enumerate() {
        let mut received: Vec<Vec<u8>> = Vec::new();
        while let Ok(Some(sample)) = subscriber.receive() {
            received.push(stamped_payload_of(&sample).to_vec());
        }
        assert_eq!(
            received.len(),
            1,
            "subscriber {i} must receive exactly one frame from a single-loan \
             fan-out (a per-connection copy loop would deliver {N}), got {}",
            received.len()
        );
        assert_eq!(
            received[0], b"fanout-payload",
            "subscriber {i} received the wrong payload",
        );
    }
}

#[test]
fn a_write_to_a_declared_but_unlinked_port_is_dropped_not_refused() {
    let inner = OutputWriterInner::new();
    inner.declare_output_ports(["video".to_string()]);

    inner
        .write_bytes_with_timestamp("video", b"data", 0, MessageGroupBoundary::StartsGroup)
        .expect("a declared port with no link drops the bag");
    assert!(!inner.has_port("video"), "dropping opens no channel");

    let refused = inner
        .write_bytes_with_timestamp("undeclared", b"data", 0, MessageGroupBoundary::StartsGroup)
        .unwrap_err();
    assert!(
        format!("{refused}").contains("Unknown output port"),
        "an undeclared port is refused by name; got {refused}"
    );
}

#[test]
fn a_write_reaches_its_port_by_any_spelling_that_casts_to_its_name() {
    let pubsub = open_channel_data_service("cast/pubsub", 2);
    let subscriber = pubsub.create_subscriber(4).unwrap();
    let inner = Arc::new(OutputWriterInner::new());
    inner.set_channel_publisher(
        "videoout",
        &pubsub,
        pubsub.create_publisher(64).unwrap(),
        ChannelEgressConfig {
            service_name: "test/videoout".to_string(),
            trust_tier: ChannelTrustTier::Trusted,
            expected_payload_bytes: 64,
            chunk_ceiling_bytes: crate::iceoryx2::TRUSTED_CHANNEL_CHUNK_CEILING_BYTES,
        },
    );
    let writer = OutputWriter::from_inner_arc(inner);

    assert!(writer.has_port("videoOut"));
    writer
        .write_bytes_with_timestamp("videoOut", b"bag", 0, MessageGroupBoundary::StartsGroup)
        .expect("a cast-alike spelling names the port");
    let sample = subscriber
        .receive()
        .unwrap()
        .expect("the write reached the channel");
    assert_eq!(stamped_payload_of(&sample), b"bag");

    let refusal = writer
        .write_bytes_with_timestamp("%%", b"bag", 0, MessageGroupBoundary::StartsGroup)
        .unwrap_err();
    assert!(
        matches!(refusal, Error::ExposedNameCastsToNothing(_)),
        "a name that casts to nothing names no port; got {refusal}"
    );
}

/// Empty (unwired) writers should fail cleanly rather than crash.
/// Mentally revert the `is_configured()` guard in `write_bytes_with_timestamp` and
/// the test segfaults dereferencing the null handle.
#[test]
fn empty_writer_fails_cleanly() {
    let writer = OutputWriter::empty();
    assert!(!writer.is_configured());
    let err = writer
        .write_bytes_with_timestamp("any_port", b"data", 0, MessageGroupBoundary::StartsGroup)
        .unwrap_err();
    let msg = format!("{}", err);
    assert!(
        msg.contains("not wired"),
        "unexpected error message: {}",
        msg
    );
    assert!(!writer.has_port("any_port"));
}

/// Clone bumps the strong count; both clones drop
/// independently. Mentally revert the strong-count bump in
/// `Clone::clone` and the second clone observes a freed handle.
#[test]
fn clone_balances_drop() {
    let inner = Arc::new(OutputWriterInner::new());
    // Bump strong count once so we can observe the post-Drop
    // strong-count drop without freeing the inner.
    let inner_for_test = inner.clone();
    let writer1 = OutputWriter::from_inner_arc(inner);
    // strong_count is 2 here: writer1's into_raw + inner_for_test.
    assert_eq!(Arc::strong_count(&inner_for_test), 2);
    let writer2 = writer1.clone();
    assert_eq!(Arc::strong_count(&inner_for_test), 3);
    drop(writer2);
    assert_eq!(Arc::strong_count(&inner_for_test), 2);
    drop(writer1);
    assert_eq!(Arc::strong_count(&inner_for_test), 1);
}

/// Full-initialization lock for the loan-direct write path: a frame
/// written after a larger one has cycled through the same publisher must
/// deliver exactly header + payload — never bytes a previous tenant left
/// in the recycled chunk.
///
/// Fail-without-fix: skip (or short-write) either region of the loan and
/// the received slice reads back the earlier frame's 0xAA filler where
/// fresh bytes belong.
#[test]
fn a_frame_written_after_a_larger_one_carries_no_stale_bytes() {
    let pubsub = open_channel_data_service("stale/pubsub", 2);
    let publisher = pubsub.create_publisher(16 * 1024).unwrap();
    let subscriber = pubsub.create_subscriber(4).unwrap();

    let inner = Arc::new(OutputWriterInner::new());
    inner.set_channel_publisher(
        "out",
        &pubsub,
        publisher,
        ChannelEgressConfig {
            service_name: "test/stale/out".to_string(),
            trust_tier: ChannelTrustTier::Trusted,
            expected_payload_bytes: 4096,
            chunk_ceiling_bytes: crate::iceoryx2::TRUSTED_CHANNEL_CHUNK_CEILING_BYTES,
        },
    );

    // A large all-0xAA frame primes the publisher's chunk pool with
    // recognisable filler, then goes back to the pool for reuse.
    let filler = vec![0xAAu8; 8 * 1024];
    inner
        .write_bytes_with_timestamp("out", &filler, 1, MessageGroupBoundary::StartsGroup)
        .unwrap();
    drop(
        subscriber
            .receive()
            .expect("receive")
            .expect("filler frame"),
    );

    let payload = b"fresh-small-payload";
    inner
        .write_bytes_with_timestamp("out", payload, 4242, MessageGroupBoundary::StartsGroup)
        .unwrap();
    let got = subscriber
        .receive()
        .expect("receive")
        .expect("fresh frame must deliver");
    assert_eq!(
        got.payload().len(),
        data_channel_payload_cache_lines_for(payload.len()),
        "the loan must be sized to exactly the payload's cache lines"
    );
    let user_header = got.user_header();
    assert_eq!(user_header.timestamp_ns, 4242);
    assert_eq!(user_header.payload_byte_len as usize, payload.len());
    assert_eq!(stamped_payload_of(&got), payload);
}
