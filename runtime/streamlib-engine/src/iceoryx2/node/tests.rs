// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use crate::core::machine_global_unique_name::mint_machine_global_unique_name_suffix;
use crate::iceoryx2::InboundLinkName;
use crate::iceoryx2::channel_sample_test_support::{TestChannelSample, stamped_payload_of};

pub(super) fn unique_service_name(tag: &str) -> String {
    format!(
        "test/node/{tag}/{}",
        mint_machine_global_unique_name_suffix()
    )
}

/// The destination-keyed notify service honors the requested `max_notifiers`
/// — exactly that many notifiers can be created and one more must fail.
/// Every source publishing into one of the destination's channels holds one
/// notifier here, so the cap is the most inbound links a destination holds.
#[test]
fn notify_service_honors_requested_max_notifiers() {
    let fanin = 3usize;
    let node = Iceoryx2Node::for_this_test_process();
    let service = node
        .open_or_create_notify_service(&unique_service_name("notify_cap"), fanin)
        .expect("open notify service");

    let mut notifiers = Vec::with_capacity(fanin);
    for i in 0..fanin {
        notifiers.push(
            service
                .create_notifier()
                .unwrap_or_else(|e| panic!("notifier {i} (under cap) must succeed: {e:?}")),
        );
    }
    assert!(
        service.create_notifier().is_err(),
        "creating notifier {} must fail — notify service was opened with \
             max_notifiers={fanin}",
        fanin + 1,
    );
}

/// A channel created for the full destination cap admits every one of its
/// subscribers — the destinations and the publisher — each opening the service
/// from its own node, as helpers do, and refuses one more.
///
/// Fail-without-fix: leave `max_nodes` at iceoryx2's default of 20 and the
/// twentieth subscriber's node cannot open the service.
#[test]
fn a_channel_admits_every_destination_and_the_tap_from_their_own_nodes_and_no_more() {
    use crate::iceoryx2::DeliveryProfile;
    use streamlib_ipc_types::{
        MAX_DESTINATIONS_PER_CHANNEL, RESERVED_PUBLISHER_SUBSCRIBER_SLOTS_PER_CHANNEL,
    };

    let max_subscribers =
        MAX_DESTINATIONS_PER_CHANNEL + RESERVED_PUBLISHER_SUBSCRIBER_SLOTS_PER_CHANNEL;
    let service_name = unique_service_name("fan_out_from_distinct_nodes");
    let open_from_a_node_of_its_own = || {
        let node = Iceoryx2Node::for_this_test_process();
        let service = node
            .open_or_create_service(
                &service_name,
                max_subscribers,
                DeliveryProfile::ORDERED_DEPTH,
            )
            .expect("every opener's node fits the service");
        (node, service)
    };
    let (_source_node, source_service) = open_from_a_node_of_its_own();
    let _publisher = source_service
        .create_publisher(64)
        .expect("the source publisher");

    let mut subscribers_on_their_own_nodes = Vec::with_capacity(max_subscribers);
    for subscriber_index in 0..max_subscribers {
        let (node, service) = open_from_a_node_of_its_own();
        let subscriber = service
            .create_subscriber(DeliveryProfile::NEWEST_DEPTH)
            .unwrap_or_else(|refusal| {
                panic!("subscriber {subscriber_index} must fit: {refusal:?}")
            });
        subscribers_on_their_own_nodes.push((node, service, subscriber));
    }

    let (_one_node_too_many, service) = open_from_a_node_of_its_own();
    assert!(
        service
            .create_subscriber(DeliveryProfile::NEWEST_DEPTH)
            .is_err(),
        "subscriber {} is one past the destination cap plus the publisher",
        max_subscribers + 1
    );
}

/// A destination's notify service created for the full inbound-link cap
/// admits a notifier from every one of those links' nodes, and refuses one
/// more.
///
/// Fail-without-fix: leave `max_nodes` at iceoryx2's event default of 36 and
/// the thirty-sixth notifier's node cannot open the service.
#[test]
fn a_notify_service_admits_every_inbound_link_from_their_own_nodes_and_no_more() {
    use streamlib_ipc_types::MAX_INBOUND_LINKS_PER_DESTINATION;

    let service_name = unique_service_name("fan_in_from_distinct_nodes");
    let open_from_a_node_of_its_own = || {
        let node = Iceoryx2Node::for_this_test_process();
        let service = node
            .open_or_create_notify_service(&service_name, MAX_INBOUND_LINKS_PER_DESTINATION)
            .expect("every opener's node fits the service");
        (node, service)
    };
    let (_destination_node, destination_service) = open_from_a_node_of_its_own();
    let _listener = destination_service
        .create_listener()
        .expect("the destination's listener");

    let mut notifiers_on_their_own_nodes = Vec::with_capacity(MAX_INBOUND_LINKS_PER_DESTINATION);
    for notifier_index in 0..MAX_INBOUND_LINKS_PER_DESTINATION {
        let (node, service) = open_from_a_node_of_its_own();
        let notifier = service
            .create_notifier()
            .unwrap_or_else(|refusal| panic!("notifier {notifier_index} must fit: {refusal:?}"));
        notifiers_on_their_own_nodes.push((node, service, notifier));
    }

    let (_one_node_too_many, service) = open_from_a_node_of_its_own();
    assert!(
        service.create_notifier().is_err(),
        "notifier {} is one past the inbound-link cap",
        MAX_INBOUND_LINKS_PER_DESTINATION + 1
    );
}

/// Every channel service states the sample limits the engine's ports stay
/// inside: a full mailbox, the most a reader may hold past it and the
/// sample being admitted borrowed per subscriber, no history, room for
/// twice its subscribers in nodes — and a publisher that loans one sample
/// at a time.
#[test]
fn a_channel_service_lends_a_mailbox_and_a_quota_per_subscriber_and_replays_none() {
    use crate::iceoryx2::DeliveryProfile;

    let max_subscribers = 3;
    let service = Iceoryx2Node::for_this_test_process()
        .open_or_create_service(
            &unique_service_name("sample_limits"),
            max_subscribers,
            DeliveryProfile::ORDERED_DEPTH,
        )
        .expect("open channel data service");

    let static_config = service.inner.static_config();
    assert_eq!(
        static_config.subscriber_max_borrowed_samples(),
        DeliveryProfile::ORDERED_DEPTH + crate::iceoryx2::MAX_INPUT_PORT_BORROW_QUOTA + 1
    );
    assert_eq!(static_config.history_size(), 0);
    assert_eq!(static_config.max_nodes(), max_subscribers * 2);

    let publisher = service.create_publisher(64).expect("the source publisher");
    let _first_loan = publisher.loan_slice_uninit(8).expect("one loan");
    assert!(
        publisher.loan_slice_uninit(8).is_err(),
        "a second loan held beside the first must be refused"
    );
}

/// A channel data service carries exactly one publisher, the source, and
/// `N + RESERVED_PUBLISHER_SUBSCRIBER_SLOTS_PER_CHANNEL` subscribers: a second
/// publisher is refused, and the subscriber past the reserved slot is too.
#[test]
fn channel_service_single_publisher_n_plus_the_reserved_slot_subscribers() {
    use streamlib_ipc_types::RESERVED_PUBLISHER_SUBSCRIBER_SLOTS_PER_CHANNEL;

    let destinations = 3usize;
    let max_subscribers = destinations + RESERVED_PUBLISHER_SUBSCRIBER_SLOTS_PER_CHANNEL;
    let node = Iceoryx2Node::for_this_test_process();
    let service = node
        .open_or_create_service(&unique_service_name("chan_caps"), max_subscribers, 4)
        .expect("open channel data service");

    // Exactly one publisher — the source.
    let _publisher = service.create_publisher(64).expect("the source publisher");
    assert!(
        service.create_publisher(64).is_err(),
        "a channel carries exactly one publisher — max_publishers drifted above \
             MAX_PUBLISHERS_PER_CHANNEL (1)",
    );

    // N destination subscribers plus the one reserved slot fit; the slot
    // after that does not.
    let mut subscribers = Vec::with_capacity(max_subscribers);
    for i in 0..max_subscribers {
        subscribers.push(service.create_subscriber(4).unwrap_or_else(|e| {
            panic!("subscriber {i} (destination or reserved) must fit: {e:?}")
        }));
    }
    assert!(
        service.create_subscriber(4).is_err(),
        "the {}th subscriber must fail — max_subscribers was N({destinations}) + \
             reserved({RESERVED_PUBLISHER_SUBSCRIBER_SLOTS_PER_CHANNEL})",
        max_subscribers + 1,
    );
}

/// A channel data service is created once and reopened by every subscriber
/// (each destination + a subprocess SDK opening the same name). iceoryx2
/// rejects reopening with a LARGER buffer than the existing service —
/// `DoesNotSupportRequestedMinBufferSize`, the exact crash the drone-racer
/// pilot hit — but accepts reopening with a SMALLER one. Channel sizing relies
/// on this: create the service at the channel's declared depth and every
/// shallower reopen fits, regardless of wiring order. If a future iceoryx2
/// changes this open-validation behavior, this test is the trip-wire.
#[test]
fn channel_service_reopen_larger_fails_smaller_succeeds() {
    let node = Iceoryx2Node::for_this_test_process();
    let subs = 2usize;

    // Bug shape: a shallow-depth open creates the service first, then a
    // deeper reopen is rejected.
    let bug_name = unique_service_name("reopen_bug");
    let _shallow = node
        .open_or_create_service(&bug_name, subs, 4)
        .expect("create channel service at depth 4");
    assert!(
        node.open_or_create_service(&bug_name, subs, 64).is_err(),
        "reopening the channel service with a deeper buffer must fail — \
             this is the DoesNotSupportRequestedMinBufferSize crash the \
             channel-depth sizing prevents",
    );

    // Fix shape: create at the deepest depth first, then every shallower
    // reopen succeeds cleanly.
    let fixed_name = unique_service_name("reopen_fixed");
    let _deep = node
        .open_or_create_service(&fixed_name, subs, 64)
        .expect("create channel service at depth 64");
    node.open_or_create_service(&fixed_name, subs, 4)
        .expect("reopening the channel service with a shallower buffer must succeed");
}

/// A reopen refused for its depth names the channel service and the buffer it
/// asked for, so a WIRE failure says which link's channel could not open and
/// why, instead of iceoryx2's bare variant.
#[test]
fn a_reopen_refused_for_its_depth_names_the_service_and_the_buffer_it_asked_for() {
    let node = Iceoryx2Node::for_this_test_process();
    let service_name = unique_service_name("reopen_refusal_named");
    let _shallow = node
        .open_or_create_service(&service_name, 2, 4)
        .expect("create channel service at depth 4");

    let Err(refusal) = node.open_or_create_service(&service_name, 2, 64) else {
        panic!("reopening the channel service with a deeper buffer must fail");
    };

    let refusal = refusal.to_string();
    assert!(refusal.contains(&format!("'{service_name}'")), "{refusal}");
    assert!(
        refusal.contains("a subscriber buffer of 64 samples"),
        "{refusal}"
    );
    assert!(
        refusal.contains("DoesNotSupportRequestedMinBufferSize"),
        "{refusal}"
    );
}

/// No link blocks a producer: the subscriber buffer auto-evicts its
/// oldest sample on overflow and the publisher's `send()` returns
/// promptly. Sends `depth * 3` samples to a depth-N service whose
/// subscriber is attached but never drains; every publish must
/// return promptly. Attaching a non-draining subscriber is
/// load-bearing — iceoryx2's publisher only observes back-pressure
/// once at least one subscriber is present (without one, samples are
/// dropped on the floor at send time regardless of the overflow
/// flag).
///
/// The behavioural half; `channel_sizing_tests` reads the same
/// contract off the opened service's static config. Neither catches
/// the `.enable_safe_overflow(true)` line simply going missing —
/// iceoryx2 defaults to `true` — but a `false` written there
/// fails both.
#[test]
fn overflow_enabled_publisher_does_not_block_on_full_buffer() {
    use std::time::{Duration, Instant};

    let depth: usize = 4;
    let node = Iceoryx2Node::for_this_test_process();
    let service = node
        .open_or_create_service(&unique_service_name("overflow_true"), 2, depth)
        .expect("open service");
    let publisher = service.create_publisher(64).expect("publisher");
    // Subscriber attached but never read — the buffer fills against
    // it. Required for the publisher to observe back-pressure at
    // all (without a subscriber, sends silently no-op).
    let _subscriber = service.create_subscriber(depth).expect("subscriber");

    let start = Instant::now();
    for _ in 0..(depth * 3) {
        TestChannelSample::carrying(0, &[0u8; 8]).send_unnumbered_on(&publisher);
    }
    let elapsed = start.elapsed();
    assert!(
        elapsed < Duration::from_millis(200),
        "overflow-on publisher must not block — sent {} samples to a depth-{} ring \
             in {:?}, expected sub-200ms",
        depth * 3,
        depth,
        elapsed,
    );
}

/// Every opener of a channel data service presents the sequence-number
/// user header. iceoryx2 refuses an opener that presents none, and a
/// service built without it is refused to the engine's own opener by a
/// message naming the header it lacks.
#[test]
fn a_channel_data_service_and_an_opener_disagreeing_on_the_user_header_are_refused_by_name() {
    let node = Iceoryx2Node::for_this_test_process();

    let built_by_the_engine = unique_service_name("header/engine-built");
    let _service = node
        .open_or_create_service(&built_by_the_engine, 2, 4)
        .expect("the engine builds the service with its header");
    let opened_without_the_header = node
        .inner
        .lock()
        .service_builder(&ServiceName::new(&built_by_the_engine).unwrap())
        .publish_subscribe::<[u8]>()
        .open();
    assert!(
            matches!(
                opened_without_the_header,
                Err(iceoryx2::service::builder::publish_subscribe::PublishSubscribeOpenError::IncompatibleTypes)
            ),
            "an opener presenting no user header must be refused: {opened_without_the_header:?}"
        );

    let built_without_the_header = unique_service_name("header/built-without");
    let _foreign_service = node
        .inner
        .lock()
        .service_builder(&ServiceName::new(&built_without_the_header).unwrap())
        .publish_subscribe::<[u8]>()
        .create()
        .unwrap();
    let refusal = match node.open_or_create_service(&built_without_the_header, 2, 4) {
        Ok(_) => panic!("a service built without the header must refuse the engine"),
        Err(refusal) => refusal.to_string(),
    };
    assert!(
        refusal.contains("DataChannelSampleUserHeader")
            && refusal.contains(&built_without_the_header),
        "the refusal must name the header and the service: {refusal}"
    );
}

/// A reopen of a live service states the depth the service was created at,
/// read off iceoryx2 rather than echoed from the call, so a later wire can
/// see how deep the channel it joins really is.
#[test]
fn a_reopened_service_states_the_depth_it_was_created_at() {
    let service_name = unique_service_name("creation_depth_stated");
    let _creating_node_service = Iceoryx2Node::for_this_test_process()
        .open_or_create_service(&service_name, 2, 42)
        .expect("create data service");

    let reopened = Iceoryx2Node::for_this_test_process()
        .open_or_create_service(&service_name, 2, 4)
        .expect("a shallower reopen joins the live service");

    assert_eq!(reopened.channel_service_creation_depth(), 42);
}

/// Opening an existing channel service finds none where nothing holds one,
/// and finds one of any depth where something does.
#[test]
fn opening_an_existing_channel_service_finds_one_of_any_depth_and_creates_none() {
    let node = Iceoryx2Node::for_this_test_process();
    let service_name = unique_service_name("open_existing");

    assert!(
        node.open_existing_channel_service(&service_name)
            .expect("a missing service is not an error")
            .is_none(),
        "nothing holds the service, so opening finds none"
    );
    assert!(
        node.open_existing_channel_service(&service_name)
            .expect("a missing service is not an error")
            .is_none(),
        "the first open created nothing"
    );

    let _held_open = node
        .open_or_create_service(&service_name, 2, 3)
        .expect("create data service");
    assert_eq!(
        node.open_existing_channel_service(&service_name)
            .expect("a live service answers")
            .map(|service| service.channel_service_creation_depth()),
        Some(3),
    );
}

/// End-to-end smoke test for a 200 Hz two-stage pipeline shape
/// (UdpSource → Decoder → Encoder → UdpSink), where each stage's drain
/// window is tight against scheduler jitter.
///
/// Producer publishes at 200 Hz to service S1; a relay thread drains
/// S1 and republishes to S2 while periodically pausing to simulate
/// downstream jitter; a consumer drains S2 and counts. We run the
/// pipeline twice:
///
/// 1. **Shallow rings (depth 4)** — during a 50 ms relay pause, the
///    producer emits ~10 messages at 200 Hz. The S1 ring (depth 4)
///    overflows; ~6 messages per pause are overwritten and lost.
/// 2. **Deep rings (depth 64, matching MavlinkMessage's declared
///    `max_queued_messages`)** — the same 10-message accumulation
///    fits comfortably; zero loss.
///
/// Reverting the engine's `subscriber_max_buffer_size` wiring to a
/// hardcoded constant would either make both runs lose the same way
/// (low constant) or both runs preserve everything (high constant) —
/// the asymmetric outcome locks the per-service plumbing.
///
/// Runtime ~1.5 s. Two thread pools per run.
#[test]
fn sustained_200hz_two_stage_relay_preserves_messages_only_with_deep_rings() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
    use std::time::{Duration, Instant};

    fn run_relay(
        s1_depth: usize,
        s2_depth: usize,
        total: u32,
        hz: u32,
        relay_pause_every: u32,
        relay_pause_ms: u64,
    ) -> (u32, u32) {
        let interval = Duration::from_micros(1_000_000 / hz as u64);
        // iceoryx2 Publishers/Subscribers are `!Send` (they hold Rc
        // internally), so each thread constructs its own ports from
        // the shared Node + service-name string.
        let node = Iceoryx2Node::for_this_test_process();
        let s1_name = Arc::new(unique_service_name("relay_s1"));
        let s2_name = Arc::new(unique_service_name("relay_s2"));

        let sent_counter = Arc::new(AtomicU32::new(0));
        // Startup barrier — iceoryx2 doesn't queue messages for late
        // subscribers, so the producer must wait until the relay
        // subscriber and consumer subscriber are both attached
        // before publishing the first sample. 3 participants:
        // producer, relay, consumer.
        let startup = Arc::new(std::sync::Barrier::new(3));
        // Two-phase shutdown: relay stops first (drain S1 → S2),
        // then consumer stops (drain S2). One shared flag would
        // race: the consumer's "drain and exit" can finish before
        // the relay republishes the last message.
        let relay_stop = Arc::new(AtomicBool::new(false));
        let consumer_stop = Arc::new(AtomicBool::new(false));

        let node_p = node.clone();
        let s1_p = s1_name.clone();
        let sent_clone = sent_counter.clone();
        let startup_p = startup.clone();
        let producer = std::thread::spawn(move || {
            let svc = node_p
                .open_or_create_service(&s1_p, 2, s1_depth)
                .expect("producer s1 open");
            let publisher = svc.create_publisher(64).expect("publisher");
            startup_p.wait();
            let start = Instant::now();
            for i in 0..total {
                TestChannelSample::carrying(0, &i.to_le_bytes()).send_unnumbered_on(&publisher);
                sent_clone.fetch_add(1, Ordering::Relaxed);

                let target = start + interval * (i + 1);
                let now = Instant::now();
                if target > now {
                    std::thread::sleep(target - now);
                }
            }
        });

        let node_r = node.clone();
        let s1_r = s1_name.clone();
        let s2_r = s2_name.clone();
        let relay_stop_t = relay_stop.clone();
        let startup_r = startup.clone();
        let relay = std::thread::spawn(move || {
            let svc_in = node_r
                .open_or_create_service(&s1_r, 2, s1_depth)
                .expect("relay s1 open");
            let svc_out = node_r
                .open_or_create_service(&s2_r, 2, s2_depth)
                .expect("relay s2 open");
            let subscriber = svc_in.create_subscriber(s1_depth).expect("relay sub");
            let publisher = svc_out.create_publisher(64).expect("relay pub");
            startup_r.wait();
            let mut count: u32 = 0;
            let relay_one = |subscriber: &ChannelDataServiceSubscriber,
                             publisher: &ChannelDataServicePublisher|
             -> bool {
                match subscriber.receive() {
                    Ok(Some(sample)) => {
                        let bytes = sample.payload().to_vec();
                        let s = publisher
                            .loan_slice_uninit(bytes.len())
                            .expect("relay loan");
                        let s = s.write_from_slice(&bytes);
                        s.send().expect("relay send");
                        true
                    }
                    _ => false,
                }
            };
            loop {
                if relay_stop_t.load(Ordering::Relaxed) {
                    // Drain anything still pending so the very last
                    // producer message makes it to s2.
                    while relay_one(&subscriber, &publisher) {}
                    break;
                }
                if relay_one(&subscriber, &publisher) {
                    count += 1;
                    if relay_pause_every > 0 && count.is_multiple_of(relay_pause_every) {
                        std::thread::sleep(Duration::from_millis(relay_pause_ms));
                    }
                } else {
                    std::thread::sleep(Duration::from_micros(200));
                }
            }
        });

        let node_c = node.clone();
        let s2_c = s2_name.clone();
        let consumer_stop_t = consumer_stop.clone();
        let startup_c = startup.clone();
        let consumer_handle = std::thread::spawn(move || {
            let svc = node_c
                .open_or_create_service(&s2_c, 2, s2_depth)
                .expect("consumer s2 open");
            let subscriber = svc.create_subscriber(s2_depth).expect("consumer sub");
            startup_c.wait();
            let mut received: u32 = 0;
            loop {
                if consumer_stop_t.load(Ordering::Relaxed) {
                    while let Ok(Some(_)) = subscriber.receive() {
                        received += 1;
                    }
                    break;
                }
                match subscriber.receive() {
                    Ok(Some(_)) => received += 1,
                    Ok(None) => std::thread::sleep(Duration::from_micros(200)),
                    Err(_) => break,
                }
            }
            received
        });

        // Phased shutdown: producer → settle → relay finishes flushing
        // S1 into S2 → settle → consumer drains S2. Using one shared
        // flag races on the very last message.
        producer.join().expect("producer thread");
        std::thread::sleep(Duration::from_millis(500));
        relay_stop.store(true, Ordering::Relaxed);
        relay.join().expect("relay thread");
        std::thread::sleep(Duration::from_millis(100));
        consumer_stop.store(true, Ordering::Relaxed);
        let received = consumer_handle.join().expect("consumer thread");

        (sent_counter.load(Ordering::Relaxed), received)
    }

    let total: u32 = 100; // 0.5 s at 200 Hz
    let hz: u32 = 200;
    let pause_every: u32 = 25;
    let pause_ms: u64 = 50;

    let (sent_shallow, recv_shallow) = run_relay(4, 4, total, hz, pause_every, pause_ms);
    let (sent_deep, recv_deep) = run_relay(64, 64, total, hz, pause_every, pause_ms);

    assert_eq!(sent_shallow, total, "producer should send every message");
    assert_eq!(sent_deep, total, "producer should send every message");

    assert!(
        recv_shallow < sent_shallow,
        "depth-4 rings should lose messages at 200 Hz under {pause_ms} ms relay pauses (every {pause_every} msgs): sent {sent_shallow}, recv {recv_shallow}"
    );
    assert_eq!(
        recv_deep, sent_deep,
        "depth-64 rings (MavlinkMessage's declared depth) should preserve every message at 200 Hz under the same jitter: sent {sent_deep}, recv {recv_deep}"
    );
}

/// End-to-end overwrite behavior: a depth-N ring published with N+1
/// unread samples drops the oldest in favor of the newest. Locks the
/// fact that the `subscriber_max_buffer_size` we pass through actually
/// reaches iceoryx2 — if the wiring is dropped (hardcoded to 16 again),
/// publishing 17+ messages would only start dropping at the much
/// larger default and this test would fail.
#[test]
fn data_service_honors_small_ring_depth_with_overwrite() {
    let depth: usize = 4;
    let send_count: usize = depth + 3; // 3 extra overwrites
    let node = Iceoryx2Node::for_this_test_process();
    let service = node
        .open_or_create_service(&unique_service_name("ring_overwrite"), 2, depth)
        .expect("open data service");

    let max_payload = 64usize;
    let publisher = service
        .create_publisher(max_payload)
        .expect("create publisher");
    let subscriber = service.create_subscriber(depth).expect("create subscriber");

    for i in 0..send_count {
        TestChannelSample::carrying(0, &[i as u8]).send_unnumbered_on(&publisher);
    }

    // Drain everything currently in the subscriber's queue.
    let mut received: Vec<u8> = Vec::new();
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(500);
    while std::time::Instant::now() < deadline {
        match subscriber.receive() {
            Ok(Some(sample)) => {
                received.push(stamped_payload_of(&sample)[0]);
            }
            Ok(None) => break,
            Err(e) => panic!("subscriber.receive() failed: {e:?}"),
        }
    }

    assert!(
        received.len() <= depth,
        "ring depth {depth} must cap the subscriber-side queue; received {} samples",
        received.len()
    );
    assert!(
        !received.is_empty(),
        "subscriber should have received at least one of the published samples"
    );
    // The newest-sent payload must be among what the subscriber drained —
    // "latest wins" semantics: even after overwrites the freshest sample
    // is preserved.
    let newest_sent = (send_count - 1) as u8;
    assert!(
        received.contains(&newest_sent),
        "newest-sent sample (payload byte {newest_sent}) should survive ring overwrites, drained: {received:?}"
    );
}

/// PowerOfTwo growth: a publisher primed with a small
/// `expected_payload_bytes` HINT must still loan — and deliver — a slice far
/// larger than the hint, because [`Iceoryx2Service::create_publisher`] opens
/// under [`iceoryx2::prelude::AllocationStrategy::PowerOfTwo`] and grows the
/// data segment on the first oversized loan; the subscriber remaps
/// transparently.
///
/// Fail-without-fix: drop the `.allocation_strategy(AllocationStrategy::PowerOfTwo)`
/// line in `create_publisher` and iceoryx2 falls back to the Static strategy,
/// so the oversized `loan_slice_uninit` fails with `ExceedsMaxLoanSize` — the
/// exact crash class this issue deletes — and the `.expect("loan")` panics.
#[test]
fn publisher_grows_segment_for_oversized_loan_and_delivers() {
    let node = Iceoryx2Node::for_this_test_process();
    let service = node
        .open_or_create_service(&unique_service_name("powertwo_growth"), 2, 4)
        .expect("open data service");

    // Prime the publisher with a deliberately tiny 64-byte hint.
    let hint_bytes = 64usize;
    let publisher = service
        .create_publisher(hint_bytes)
        .expect("create PowerOfTwo publisher");
    let subscriber = service.create_subscriber(4).expect("subscriber");

    // Loan a 1 MiB slice — ~16000x the primed slot. Under Static this is an
    // ExceedsMaxLoanSize failure; under PowerOfTwo it grows and succeeds.
    let oversized = 1024 * 1024usize;
    let mut payload = vec![0u8; oversized];
    payload[0] = 0xAB;
    payload[oversized - 1] = 0xCD;

    TestChannelSample::carrying(0, &payload).send_unnumbered_on(&publisher);

    let received = subscriber
        .receive()
        .expect("receive")
        .expect("subscriber must transparently remap the grown segment and deliver");
    let received = stamped_payload_of(&received);
    assert_eq!(received.len(), oversized, "full payload delivered");
    assert_eq!(received[0], 0xAB);
    assert_eq!(received[oversized - 1], 0xCD);
}

/// End-to-end reconnect cycle: connect → disconnect → reconnect a
/// persistent source+destination pair the way the compiler op wires it, and
/// prove the per-link reclaim releases both iceoryx2 services so the reconnect
/// recreates them fresh — reproducing (and defeating) BOTH reported errors:
///
/// - `ExceedsMaxSupportedNotifiers`: the notify service is created with
///   `max_notifiers = fan-in (1)`. Without reclaim, the first connect's
///   notifier stays live on [`OutputWriterInner`], so the reconnect's
///   `create_notifier` is the SECOND on a max-1 service and fails.
/// - `DoesNotSupportRequestedMinBufferSize`: the reconnect opens the data
///   service at a DEEPER ring depth. Without reclaim, the stale shallow
///   service is still held by the leaked publisher/subscriber, so the deeper
///   reopen is rejected.
///
/// Fail-without-fix: revert `OutputWriterInner::remove_channel_link` /
/// `InputMailboxesInner::remove_channel_link` to no-ops and the reconnect's `create_notifier`
/// trips `ExceedsMaxSupportedNotifiers` and the deeper `open_or_create_service`
/// trips `DoesNotSupportRequestedMinBufferSize` — the two `.expect`s panic.
#[test]
fn disconnect_reconnect_cycle_reclaims_notifier_and_data_service() {
    use crate::iceoryx2::{
        ChannelEgressConfig, ChannelTrustTier, InputMailboxesInner, OutputWriterInner, ReadMode,
        TRUSTED_CHANNEL_CHUNK_CEILING_BYTES,
    };
    use streamlib_ipc_types::RESERVED_PUBLISHER_SUBSCRIBER_SLOTS_PER_CHANNEL;

    let node = Iceoryx2Node::for_this_test_process();
    let data_name = unique_service_name("cycle/data");
    let notify_name = unique_service_name("cycle/notify");
    let link_id = "L-cycle-1";
    // fan-in = 1 (one inbound link), fan-out = 1 (+ the reserved slot).
    let max_notifiers = 1usize;
    let max_subscribers = 1 + RESERVED_PUBLISHER_SUBSCRIBER_SLOTS_PER_CHANNEL;

    let out_inner = Arc::new(OutputWriterInner::new());
    let in_inner = Arc::new(InputMailboxesInner::new());

    // One `connect()` wiring pass at ring depth `depth`, exactly as the
    // compiler op stitches a Rust→Rust link.
    let connect = |depth: usize| {
        let data = node
            .open_or_create_service(&data_name, max_subscribers, depth)
            .expect("open channel data service");
        let notify = node
            .open_or_create_notify_service(&notify_name, max_notifiers)
            .expect("open notify service");

        if !out_inner.has_channel_publisher("out") {
            let publisher = data.create_publisher(64).expect("source publisher");
            out_inner.set_channel_publisher(
                "out",
                &data,
                publisher,
                ChannelEgressConfig {
                    service_name: data_name.clone(),
                    trust_tier: ChannelTrustTier::Trusted,
                    expected_payload_bytes: 64,
                    chunk_ceiling_bytes: TRUSTED_CHANNEL_CHUNK_CEILING_BYTES,
                },
            );
        }
        out_inner.add_channel_link(
            "out",
            link_id,
            notify.create_notifier().expect(
                "create_notifier must fit the notify service's max_notifiers cap — a \
                     leaked notifier from the previous connect would trip \
                     ExceedsMaxSupportedNotifiers here",
            ),
        );

        if !in_inner.has_port("in") {
            in_inner.add_port("in", depth, ReadMode::ReadNextInOrder);
        }
        in_inner.add_channel_subscriber(
            "in",
            link_id,
            &InboundLinkName::from("psource/out"),
            data.create_subscriber(depth).expect("dest subscriber"),
        );
        if !in_inner.has_listener() {
            in_inner.set_listener(notify.create_listener().expect("dest listener"));
        }
    };

    // Simulate `close_iceoryx2_service`'s per-link reclaim on both halves.
    let disconnect = || {
        out_inner.remove_channel_link("out", link_id);
        in_inner.remove_channel_link(link_id);
    };

    // First connect at a shallow depth.
    connect(4);
    assert!(out_inner.has_channel_publisher("out"));
    assert!(in_inner.has_listener());

    // Disconnect: the reclaim must drop the publisher, notifier, subscriber
    // and listener so iceoryx2 releases both services.
    disconnect();
    assert!(
        !out_inner.has_channel_publisher("out"),
        "the channel publisher must be released on disconnect",
    );
    assert!(
        !in_inner.has_listener(),
        "the destination listener must be released on disconnect",
    );

    // Reconnect at a DEEPER ring depth. Both `.expect`s inside `connect`
    // (create_notifier + open_or_create_service) are the trip-wires.
    connect(64);
    assert!(out_inner.has_channel_publisher("out"));
    assert!(in_inner.has_listener());
}
