// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

/// A reader that subscribes after a write is one that write missed, until
/// the next write reaches it; a reader leaving is not.
#[test]
fn a_reader_subscribing_after_a_write_is_one_the_last_write_missed() {
    let pubsub = open_channel_data_service("late-reader", 3);
    let inner = output_writer_with_one_channel(
        &pubsub,
        crate::iceoryx2::TRUSTED_CHANNEL_CHUNK_CEILING_BYTES,
    );
    assert!(
        !inner.has_a_reader_the_last_write_missed("out"),
        "no reader yet"
    );
    assert!(
        !inner.has_a_reader_the_last_write_missed("undeclared"),
        "a port with no channel has no reader"
    );

    let first_reader = pubsub.create_subscriber(4).unwrap();
    assert!(
        inner.has_a_reader_the_last_write_missed("out"),
        "nothing has been written to the first reader"
    );
    inner
        .write_bytes_with_timestamp("out", b"picture", 0, MessageGroupBoundary::StartsGroup)
        .unwrap();
    assert!(!inner.has_a_reader_the_last_write_missed("out"));

    let late_reader = pubsub.create_subscriber(4).unwrap();
    assert!(inner.has_a_reader_the_last_write_missed("out"));
    assert!(
        late_reader.receive().unwrap().is_none(),
        "the channel keeps no history for a late reader"
    );
    inner
        .write_bytes_with_timestamp("out", b"picture", 1, MessageGroupBoundary::StartsGroup)
        .unwrap();
    assert!(!inner.has_a_reader_the_last_write_missed("out"));
    assert!(late_reader.receive().unwrap().is_some());

    drop(first_reader);
    assert!(
        !inner.has_a_reader_the_last_write_missed("out"),
        "a reader leaving is no reader the write missed"
    );
}

/// Per-link source reclaim: a source port feeding two destination
/// links that both wait on a listener holds two tagged notifiers on ONE
/// channel egress. Disconnecting one link drops only its notifier (the
/// channel — and its publisher — survive so the other destination keeps
/// receiving); disconnecting the last link removes the whole channel egress,
/// releasing the publisher so a reconnect recreates a fresh-sized service.
///
/// Fail-without-fix: make `remove_channel_link` a no-op and the first
/// removal leaves both notifiers, so `has_channel_publisher` stays true
/// after the final disconnect and the "channel fully removed" assertion
/// fails.
#[test]
fn remove_channel_link_reclaims_per_link_then_drops_channel() {
    let node = crate::iceoryx2::create_iceoryx2_node_for_this_test_process();
    let reclaim_channel = open_channel_data_service("reclaim/pubsub", 4);
    let publisher = reclaim_channel.create_publisher(4096).unwrap();

    let inner = Arc::new(OutputWriterInner::new());
    inner.set_channel_publisher(
        "out",
        &reclaim_channel,
        publisher,
        ChannelEgressConfig {
            service_name: "test/reclaim/out".to_string(),
            trust_tier: ChannelTrustTier::Trusted,
            expected_payload_bytes: 4096,
            chunk_ceiling_bytes: crate::iceoryx2::TRUSTED_CHANNEL_CHUNK_CEILING_BYTES,
        },
    );

    let notify = |tag: &str| {
        node.service_builder(&ServiceName::new(&unique_suffix(tag)).unwrap())
            .event()
            .max_notifiers(2)
            .max_listeners(1)
            .open_or_create()
            .unwrap()
            .notifier_builder()
            .create()
            .unwrap()
    };
    inner.add_channel_link("out", "L-link-a", notify("reclaim/notify/a"));
    inner.add_channel_link("out", "L-link-b", notify("reclaim/notify/b"));
    assert!(inner.has_channel_publisher("out"));

    // Disconnect one of two links: the channel (and publisher) survive.
    assert!(
        !inner.remove_channel_link("out", "L-link-a"),
        "removing one of two links must NOT drop the shared channel publisher",
    );
    assert!(
        inner.has_channel_publisher("out"),
        "the channel must stay wired while link-b is still connected",
    );

    // A stale/unknown link id is a no-op that keeps the channel.
    assert!(!inner.remove_channel_link("out", "L-does-not-exist"));
    assert!(inner.has_channel_publisher("out"));

    // Disconnect the last link: the whole channel egress is reclaimed.
    assert!(
        inner.remove_channel_link("out", "L-link-b"),
        "removing the last outbound link must drop the channel publisher",
    );
    assert!(
        !inner.has_channel_publisher("out"),
        "the publisher (and its data service) must be released after the final \
         disconnect so a reconnect recreates a fresh-sized service",
    );
}

/// Per-channel ceiling + PowerOfTwo growth: a payload UNDER the
/// channel ceiling but far over the primed slot grows the segment and
/// delivers intact; a payload OVER the ceiling is refused with the named
/// [`Error::PayloadExceedsChannelCeiling`], counted, and the stream
/// continues (a subsequent in-bounds write still delivers).
///
/// Fail-without-fix: remove the ceiling branch in `write_bytes_with_timestamp` and the
/// over-ceiling write returns `Ok` (no error, count stays 0) — the graceful
/// refusal this issue adds is gone. Remove the growth/PowerOfTwo path and the
/// 100 KiB loan fails instead of delivering.
#[test]
fn write_bytes_with_timestamp_refuses_over_ceiling_and_grows_within_it() {
    let pubsub = open_channel_data_service("ceiling/pubsub", 2);
    // Prime tiny (4 KiB) under PowerOfTwo so a 100 KiB write must grow.
    let publisher = pubsub.create_publisher(4096).unwrap();
    let subscriber = pubsub.create_subscriber(4).unwrap();

    let inner = Arc::new(OutputWriterInner::new());
    let ceiling = 128 * 1024usize;
    inner.set_channel_publisher(
        "out",
        &pubsub,
        publisher,
        ChannelEgressConfig {
            service_name: "test/ceiling/out".to_string(),
            trust_tier: ChannelTrustTier::UntrustedSession,
            expected_payload_bytes: 64,
            chunk_ceiling_bytes: ceiling,
        },
    );

    // Within ceiling but far above the primed 4 KiB slot — grows + delivers.
    let within = vec![0x5Au8; 100 * 1024];
    inner
        .write_bytes_with_timestamp("out", &within, 111, MessageGroupBoundary::StartsGroup)
        .expect("in-bounds payload must grow the segment and send");
    assert_eq!(
        refused_bag_count_of(&inner, "out"),
        0,
        "an in-bounds payload must not be counted as refused"
    );
    let got = subscriber
        .receive()
        .expect("receive")
        .expect("grown in-bounds frame must be delivered");
    assert_eq!(stamped_payload_of(&got).len(), within.len());
    drop(got);

    // Above ceiling — refused with the named error + counted, never a panic.
    let over = vec![0u8; ceiling + 1];
    let err = inner
        .write_bytes_with_timestamp("out", &over, 222, MessageGroupBoundary::StartsGroup)
        .expect_err("a payload above the channel ceiling must be refused");
    match err {
        Error::PayloadExceedsChannelCeiling {
            ref channel,
            payload_bytes,
            largest_admitted_frame_bytes,
            chunk_ceiling_bytes,
            tier,
            refused_bags_on_the_output_port,
        } => {
            assert_eq!(channel, "test/ceiling/out");
            assert_eq!(payload_bytes, over.len());
            // Both numbers, because they are not the same one: the producer
            // fits inside the frame ceiling, the operator raises the chunk
            // ceiling, and the refusal has to name each as itself.
            assert_eq!(
                largest_admitted_frame_bytes,
                streamlib_ipc_types::largest_channel_frame_bytes_under_a_chunk_ceiling(ceiling)
            );
            assert_eq!(chunk_ceiling_bytes, ceiling);
            assert!(largest_admitted_frame_bytes < chunk_ceiling_bytes);
            assert_eq!(tier, ChannelTrustTierLabel::UntrustedSession);
            assert_eq!(refused_bags_on_the_output_port, 1);
        }
        other => panic!("expected PayloadExceedsChannelCeiling, got {other:?}"),
    }
    assert_eq!(
        refused_bag_count_of(&inner, "out"),
        1,
        "the refused sample must be counted"
    );

    // Stream continues: a subsequent in-bounds write still delivers.
    inner
        .write_bytes_with_timestamp(
            "out",
            b"still-alive",
            333,
            MessageGroupBoundary::StartsGroup,
        )
        .expect("stream must continue after a refusal");
    let got = subscriber
        .receive()
        .expect("receive")
        .expect("post-refusal frame must be delivered");
    assert_eq!(stamped_payload_of(&got), b"still-alive");
}

/// A helper's refusals and sends reach its board as they are counted, and a
/// port whose channel was released and opened again mirrors from zero
/// through its new claim alone.
///
/// Fail-without-fix: mirror nothing at `write_bytes_with_timestamp`, and the
/// entry still reads zero after two refusals and a send.
#[test]
fn refusals_and_sends_on_a_mirrored_output_port_reach_its_board_entry_as_they_are_counted() {
    let (board, board_writer, _helper_node) =
        crate::iceoryx2::a_loss_count_board_and_its_helpers_writer_for_this_test_process(
            &["out"],
            &[],
        );
    let bags_on_the_board = || {
        board.output_port_entry("out").map(|entry| {
            (
                entry.wiring_generation,
                entry.refused_bags,
                entry.written_bags,
            )
        })
    };
    let pubsub = open_channel_data_service("mirrored-refusal", 2);
    let inner = OutputWriterInner::new();
    let install_the_channel = |wiring_generation: u64| {
        inner.set_channel_publisher(
            "out",
            &pubsub,
            pubsub.create_publisher(64).unwrap(),
            ChannelEgressConfig {
                service_name: "test/mirrored-refusal/out".to_string(),
                trust_tier: ChannelTrustTier::UntrustedSession,
                expected_payload_bytes: 64,
                chunk_ceiling_bytes: 1024,
            },
        );
        inner.add_channel_link(
            "out",
            "L-out",
            crate::iceoryx2::a_notifier_nothing_listens_on(&unique_suffix(
                "mirrored-refusal/notify",
            )),
        );
        inner
            .mirror_an_output_ports_bag_counts_into(
                "out",
                board_writer
                    .claim_output_port_entry("out", wiring_generation)
                    .expect("the board carries the declared port"),
            )
            .expect("a port with a channel mirrors its counts");
    };
    let over_the_ceiling = vec![0u8; 2048];

    install_the_channel(1);
    inner
        .write_bytes_with_timestamp(
            "out",
            &over_the_ceiling,
            0,
            MessageGroupBoundary::StartsGroup,
        )
        .unwrap_err();
    inner
        .write_bytes_with_timestamp(
            "out",
            &over_the_ceiling,
            1,
            MessageGroupBoundary::StartsGroup,
        )
        .unwrap_err();
    inner
        .write_bytes_with_timestamp("out", b"sent", 2, MessageGroupBoundary::StartsGroup)
        .unwrap();
    assert_eq!(bags_on_the_board(), Some((1, 2, 1)));

    assert!(inner.remove_channel_link("out", "L-out"));
    install_the_channel(2);
    assert_eq!(
        bags_on_the_board(),
        Some((2, 0, 0)),
        "a port's reopened channel counts from zero, as its native count does"
    );
    inner
        .write_bytes_with_timestamp(
            "out",
            &over_the_ceiling,
            2,
            MessageGroupBoundary::StartsGroup,
        )
        .unwrap_err();
    assert_eq!(bags_on_the_board(), Some((2, 1, 0)));

    assert!(
        inner
            .mirror_an_output_ports_bag_counts_into(
                "never-opened",
                board_writer.claim_output_port_entry("out", 3).unwrap(),
            )
            .is_err(),
        "a port with no channel has no bag counts to mirror"
    );
}

/// Every send that may deliver carries the port's next sequence number,
/// and a write refused at the ceiling carries none: the subscriber reads
/// consecutive numbers either side of the refusal, so the refused bag —
/// which belongs to no link — is never read as a ring overwrite too.
///
/// Fail-without-fix: advance the number on a refusal and the second bag
/// arrives numbered 2, a gap its destination would count.
#[test]
fn a_write_refused_at_the_ceiling_consumes_no_sequence_number() {
    let pubsub = open_channel_data_service("sequence/pubsub", 2);
    let subscriber = pubsub.create_subscriber(4).unwrap();
    let ceiling = 1024usize;
    let inner = output_writer_with_one_channel(&pubsub, ceiling);

    inner
        .write_bytes_with_timestamp("out", b"first", 1, MessageGroupBoundary::StartsGroup)
        .unwrap();
    inner
        .write_bytes_with_timestamp(
            "out",
            &vec![0u8; ceiling],
            2,
            MessageGroupBoundary::StartsGroup,
        )
        .expect_err("a frame past the ceiling is refused");
    inner
        .write_bytes_with_timestamp("out", b"second", 3, MessageGroupBoundary::StartsGroup)
        .unwrap();

    let mut sequence_numbers = Vec::new();
    while let Some(sample) = subscriber.receive().unwrap() {
        sequence_numbers.push(sample.user_header().sequence_number);
    }
    assert_eq!(sequence_numbers, vec![0, 1]);
    assert_eq!(refused_bag_count_of(&inner, "out"), 1);
    assert_eq!(
        written_bag_count_of(&inner, "out"),
        2,
        "a refused write is never counted as written"
    );
}

/// Groups declared given up are counted on the port at once and ride the
/// next send that reaches a subscriber, past a write the ceiling refused,
/// and are carried once.
#[test]
fn a_declared_upstream_loss_rides_the_next_send_past_a_refused_write_and_is_carried_once() {
    let pubsub = open_channel_data_service("declared/pubsub", 2);
    let subscriber = pubsub.create_subscriber(4).unwrap();
    let ceiling = 1024usize;
    let inner = output_writer_with_one_channel(&pubsub, ceiling);

    inner
        .declare_groups_given_up_upstream_before_the_next_write("out", 2)
        .unwrap();
    assert_eq!(
        inner
            .bag_counts_by_output_port()
            .given_up_group_count_snapshot_by_output_port(),
        std::collections::BTreeMap::from([("out".to_string(), 2)]),
        "counted on the producer before any write carries it"
    );
    inner
        .write_bytes_with_timestamp(
            "out",
            &vec![0u8; ceiling],
            1,
            MessageGroupBoundary::StartsGroup,
        )
        .expect_err("a frame past the ceiling is refused");
    inner
        .write_bytes_with_timestamp("out", b"first", 2, MessageGroupBoundary::StartsGroup)
        .unwrap();
    inner
        .write_bytes_with_timestamp("out", b"second", 3, MessageGroupBoundary::StartsGroup)
        .unwrap();

    let mut numbering = Vec::new();
    while let Some(sample) = subscriber.receive().unwrap() {
        let user_header = *sample.user_header();
        numbering.push((
            user_header.sequence_number,
            user_header.messages_lost_upstream_before_this_one,
        ));
    }
    assert_eq!(numbering, vec![(0, 2), (1, 0)]);
}

/// A declaration on a port no link has reached counts nothing and sends
/// nothing, as a write to it does; an undeclared port is refused.
#[test]
fn a_declared_upstream_loss_on_an_unlinked_declared_port_is_accepted_and_counts_nothing() {
    let inner = OutputWriterInner::new();
    inner.declare_output_ports(["out".to_string()]);

    inner
        .declare_groups_given_up_upstream_before_the_next_write("out", 1)
        .unwrap();
    assert!(
        inner
            .bag_counts_by_output_port()
            .given_up_group_count_snapshot_by_output_port()
            .is_empty()
    );
    assert!(
        inner
            .declare_groups_given_up_upstream_before_the_next_write("not_declared", 1)
            .is_err()
    );
}

/// A send consumes its number once it may have reached a subscriber, and
/// only a failure known to precede any delivery gives it back.
/// `ConnectionError` wraps a type from a crate the engine does not depend
/// on, so it is placed by the exhaustive match alone.
#[test]
fn a_send_consumes_its_sequence_number_unless_it_failed_before_any_delivery() {
    use iceoryx2::port::LoanError;

    assert!(a_send_may_have_delivered(&Ok(1)));
    assert_eq!(sequence_number_following_a_send(7, true), 8);
    assert_eq!(sequence_number_following_a_send(7, false), 7);
    assert_eq!(
        sequence_number_following_a_send(u64::MAX, true),
        0,
        "the last number wraps to zero rather than overflowing in a debug build"
    );
    assert!(
        a_send_may_have_delivered(&Ok(0)),
        "a send that reached no subscriber still numbered a bag"
    );
    for failed_before_delivering in [
        SendError::ConnectionBrokenSinceSenderNoLongerExists,
        SendError::LoanError(LoanError::OutOfMemory),
    ] {
        assert!(
            !a_send_may_have_delivered(&Err(failed_before_delivering)),
            "{failed_before_delivering:?} reached no subscriber"
        );
    }
    for failed_after_delivering_to_some in [
        SendError::UnableToDeliver,
        SendError::ConnectionCorrupted,
        SendError::InternalError,
    ] {
        assert!(
            a_send_may_have_delivered(&Err(failed_after_delivering_to_some)),
            "{failed_after_delivering_to_some:?} may have reached a subscriber"
        );
    }
}

/// A port's refusals are counted for as long as its channel stays open, and
/// go with the publisher when its last link does.
#[test]
fn an_output_ports_refusals_leave_with_its_last_link() {
    let pubsub = open_channel_data_service("refusals-leave/pubsub", 2);
    let inner = output_writer_with_one_channel(&pubsub, 128);
    inner.add_channel_link(
        "out",
        "L-only",
        crate::iceoryx2::a_notifier_nothing_listens_on(&unique_suffix("refusals-leave/notify")),
    );
    let bag_counts = inner.bag_counts_by_output_port();
    inner
        .write_bytes_with_timestamp("out", &[0u8; 128], 0, MessageGroupBoundary::StartsGroup)
        .unwrap_err();
    assert_eq!(refused_bag_count_of(&inner, "out"), 1);

    assert!(inner.remove_channel_link("out", "L-only"));

    assert!(
        bag_counts
            .refused_bag_count_snapshot_by_output_port()
            .is_empty()
    );
}

/// Drift guard for the two trust-tier spellings: `ChannelTrustTier::as_str`
/// (ipc-types) and `ChannelTrustTierLabel`'s `Display` (the engine-free error
/// crate) name the tier independently — a forced layering cost (the orphan
/// rule + engine purity keep the error crate off ipc-types), not duplication
/// to merge. The engine is the one place that sees both, so it locks the two
/// string forms — and `trust_tier_label`'s mapping between them — so they
/// can't silently drift.
#[test]
fn trust_tier_label_spellings_do_not_drift() {
    for tier in [
        ChannelTrustTier::Trusted,
        ChannelTrustTier::UntrustedSession,
    ] {
        let label = trust_tier_label(tier);
        assert_eq!(
            tier.as_str(),
            label.to_string(),
            "ChannelTrustTier::as_str must match ChannelTrustTierLabel's Display",
        );
    }
    // Pin the exact variant pairing too, so a swapped mapping is caught.
    assert_eq!(
        ChannelTrustTier::Trusted.as_str(),
        ChannelTrustTierLabel::Trusted.to_string()
    );
    assert_eq!(
        ChannelTrustTier::UntrustedSession.as_str(),
        ChannelTrustTierLabel::UntrustedSession.to_string()
    );
}
