// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

#[test]
fn a_windowed_port_reports_data_only_once_a_full_window_can_be_emitted() {
    let (publisher, subscriber) = open_channel_for_one_link("window/readiness", 8);

    let mailboxes = InputMailboxesInner::new();
    mailboxes.add_windowed_port(
        "in",
        ReadMode::ReadNextInOrder,
        a_512_512_contract_at(16_000, 1),
    );
    mailboxes.add_channel_subscriber(
        "in",
        "L-only",
        &InboundLinkName::from("ponly/out"),
        subscriber,
    );

    // A third of a window is not a window.
    publish_one_mono_channels_block(&publisher, 160, 16_000, 0);
    assert!(
        !mailboxes.has_data("in"),
        "160 of 512 samples is not a window, and a wake here would dispatch a \
         process() with nothing to read"
    );
    assert!(!mailboxes.any_port_has_data());

    // Enough to complete one.
    for block in 1..4u64 {
        publish_one_mono_channels_block(
            &publisher,
            160,
            16_000,
            (block * 160 * 1_000_000_000 / 16_000) as i64,
        );
    }
    assert!(
        mailboxes.has_data("in"),
        "640 samples completes a 512 window"
    );
    assert!(mailboxes.any_port_has_data());

    let window = mailboxes
        .read_raw("in")
        .expect("a window reads")
        .expect("the gate said a window was ready")
        .payload;
    assert!(!window.is_empty());
}

/// One 1024-sample quantum against a 512/512 contract satisfies exactly two
/// windows — the count the reactive drain loop dispatches `process()`.
#[test]
fn one_1024_sample_quantum_reads_out_of_a_512_512_port_exactly_twice() {
    let (publisher, subscriber) = open_channel_for_one_link_loaning("window/quantum", 4, 16_384);

    let mailboxes = InputMailboxesInner::new();
    mailboxes.add_windowed_port(
        "in",
        ReadMode::ReadNextInOrder,
        a_512_512_contract_at(16_000, 1),
    );
    mailboxes.add_channel_subscriber(
        "in",
        "L-only",
        &InboundLinkName::from("ponly/out"),
        subscriber,
    );
    publish_one_mono_channels_block(&publisher, 1024, 16_000, 0);

    let mut windows = 0;
    while mailboxes.any_port_has_data() {
        assert!(
            mailboxes.read_raw("in").expect("reads").is_some(),
            "the gate promised a window; the read must produce one"
        );
        windows += 1;
        assert!(
            windows <= 4,
            "a 1024-sample quantum cannot fill more windows"
        );
    }
    assert_eq!(windows, 2);
}

/// A port with no contract is unchanged in every respect: the bytes a
/// reader gets back are the bytes the producer published.
#[test]
fn a_contract_less_port_still_reads_the_bag_the_producer_published_byte_for_byte() {
    let (publisher, subscriber) = open_channel_for_one_link_loaning("window/untouched", 4, 16_384);

    let mailboxes = InputMailboxesInner::new();
    mailboxes.add_port("in", 8, ReadMode::ReadNextInOrder);
    mailboxes.add_channel_subscriber(
        "in",
        "L-only",
        &InboundLinkName::from("ponly/out"),
        subscriber,
    );

    let published = TestChannelSample::of_a_mono_channels_block(1024, 16_000, 77);
    publisher.send_numbered(&published);

    assert!(
        mailboxes.has_data("in"),
        "an arrived bag is data on an unwindowed port"
    );
    let read_back = mailboxes
        .read_raw("in")
        .expect("reads")
        .expect("a bag")
        .payload;
    assert_eq!(read_back, published.carried_body);
}

/// A loss declared upstream reaches an `audio_window` port's reader on the
/// window cut from the block that carried it.
#[test]
fn a_declared_upstream_loss_reaches_the_window_cut_from_the_block_carrying_it() {
    let channel = crate::iceoryx2::Iceoryx2Node::for_this_test_process()
        .open_or_create_service(&unique_suffix("declared/window"), 2, 16)
        .unwrap();
    let mailboxes = InputMailboxesInner::new();
    mailboxes.add_windowed_port(
        "in",
        ReadMode::ReadNextInOrder,
        a_512_512_contract_at(16_000, 1),
    );
    mailboxes.add_channel_subscriber(
        "in",
        "L-declared",
        &InboundLinkName::from("pmic/audio"),
        channel.create_subscriber(16).unwrap(),
    );
    let output_writer = output_writer_publishing_onto(&channel, "audio");
    let write_block_at = |first_sample_timestamp_ns: i64| {
        let block =
            TestChannelSample::of_a_mono_channels_block(512, 16_000, first_sample_timestamp_ns);
        output_writer
            .write_bytes_described_by(
                "audio",
                &block.carried_body,
                first_sample_timestamp_ns,
                MessageGroupBoundary::StartsGroup,
                block.payload_descriptor,
            )
            .unwrap();
    };

    write_block_at(0);
    output_writer
        .declare_groups_given_up_upstream_before_the_next_write("audio", 1)
        .unwrap();
    write_block_at(64_000_000);

    let losses_told_per_window: Vec<u64> = std::iter::from_fn(|| mailboxes.read_raw("in").unwrap())
        .map(|window| window.lost_since_last_read)
        .collect();
    assert_eq!(losses_told_per_window, [0, 1]);
}

/// The accumulator is not a second drop site: bags stay in the counted
/// mailbox until a read takes one, so a windowed port under overrun counts
/// exactly what an unwindowed one does.
#[test]
fn a_windowed_ports_per_link_drop_counts_match_an_unwindowed_ports_under_the_same_overrun() {
    let contract = a_512_512_contract_at(16_000, 1);
    let depth = contract.windowed_port_mailbox_depth();
    let published_bags = depth + 7;

    fn losses_under_overrun(
        tag: &str,
        depth: usize,
        published_bags: usize,
        contract: Option<ResolvedAudioWindowContract>,
    ) -> u64 {
        let (publisher, subscriber) = open_channel_for_one_link(tag, depth);
        let mailboxes = InputMailboxesInner::new();
        match contract {
            Some(contract) => {
                mailboxes.add_windowed_port("in", ReadMode::ReadNextInOrder, contract)
            }
            None => mailboxes.add_port("in", depth, ReadMode::ReadNextInOrder),
        }
        mailboxes.add_channel_subscriber(
            "in",
            "L-only",
            &InboundLinkName::from("ponly/out"),
            subscriber,
        );

        for block in 0..published_bags {
            publish_one_mono_channels_block(&publisher, 160, 16_000, (block as i64) * 10_000_000);
            // One at a time so the subscriber ring never overflows and every
            // loss lands at the mailbox, where it is counted.
            mailboxes.receive_pending();
        }
        mailboxes
            .dropped_bag_counts_by_inbound_link()
            .dropped_bag_count_snapshot_by_inbound_link()
            .values()
            .sum()
    }

    let windowed = losses_under_overrun("window/counted", depth, published_bags, Some(contract));
    let unwindowed = losses_under_overrun("window/uncounted", depth, published_bags, None);

    assert_eq!(
        windowed, 7,
        "a mailbox {depth} deep loses the {published_bags} minus {depth}"
    );
    assert_eq!(
        windowed, unwindowed,
        "windowing must not add or hide a loss the counters do not see"
    );
}

/// A window wider than the delivery profile's depth sizes its own mailbox,
/// because `ORDERED_DEPTH` cannot hold a one-second rolling window's quanta.
#[test]
fn a_long_windows_mailbox_is_sized_from_its_contract_rather_than_the_profiles_depth() {
    let one_second_rolling = ResolvedAudioWindowContract::from_declared_values(
        &crate::core::descriptors::AudioWindowContractDeclaredValues {
            sample_rate: 16_000,
            channels: Some(1),
            dtype: "f32".to_string(),
            window_size: 16_000,
            hop: 160,
        },
    )
    .expect("a one-second rolling window is legal");

    let (publisher, subscriber) = open_channel_for_one_link("window/depth", 4);
    let mailboxes = InputMailboxesInner::new();
    mailboxes.add_windowed_port("in", ReadMode::ReadNextInOrder, one_second_rolling);
    mailboxes.add_channel_subscriber(
        "in",
        "L-only",
        &InboundLinkName::from("ponly/out"),
        subscriber,
    );

    // Deeper than ORDERED_DEPTH: publishing that many must lose nothing.
    for block in 0..(crate::iceoryx2::DeliveryProfile::ORDERED_DEPTH + 8) {
        publish_one_mono_channels_block(&publisher, 160, 16_000, (block as i64) * 10_000_000);
        mailboxes.receive_pending();
    }

    let lost: u64 = mailboxes
        .dropped_bag_counts_by_inbound_link()
        .dropped_bag_count_snapshot_by_inbound_link()
        .values()
        .sum();
    assert_eq!(
        lost, 0,
        "a one-second window's port must hold more than the profile's 16 blocks"
    );
}

/// A bag the stage cannot read is refused by name at the read, naming the
/// port — never reshaped into a plausible wrong answer.
#[test]
fn a_bag_the_stage_cannot_read_is_refused_at_the_read_naming_the_port() {
    let (publisher, subscriber) = open_channel_for_one_link("window/refusal", 4);

    let mailboxes = InputMailboxesInner::new();
    mailboxes.add_windowed_port(
        "in",
        ReadMode::ReadNextInOrder,
        a_512_512_contract_at(16_000, 1),
    );
    mailboxes.add_channel_subscriber(
        "in",
        "L-only",
        &InboundLinkName::from("ponly/out"),
        subscriber,
    );

    let not_an_audio_block =
        rmp_serde::to_vec_named(&std::collections::BTreeMap::from([("width", 1920)]))
            .expect("encodes");
    publish_one_frame(&publisher, "mic_out", &not_an_audio_block);
    mailboxes.receive_pending();

    let refusal = mailboxes
        .read_raw("in")
        .expect_err("a bag with no audio-block keys is refused");
    let rendered = refusal.to_string();
    assert!(
        rendered.contains("'in'") && rendered.contains("audio block"),
        "the refusal must name the port and what it could not read; got {rendered}"
    );
}

/// The negative half: a consumer that keeps up loses nothing, and its
/// wired links say so with a zero rather than by going missing. A counter
/// that only appears once a link has lost something is indistinguishable
/// from a link nobody wired.
#[test]
fn a_port_that_keeps_up_reports_a_zero_for_every_wired_link() {
    let (publisher_a, subscriber_a) = open_channel_for_one_link("no-drop/a", 4);
    let (publisher_b, subscriber_b) = open_channel_for_one_link("no-drop/b", 4);

    let mailboxes = InputMailboxesInner::new();
    mailboxes.add_port("in", 8, ReadMode::ReadNextInOrder);
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

    for _ in 0..2 {
        publish_one_frame(&publisher_a, "src_a_out", b"from-a");
        mailboxes.receive_pending();
        publish_one_frame(&publisher_b, "src_b_out", b"from-b");
        mailboxes.receive_pending();
    }

    assert_eq!(
        mailboxes
            .dropped_bag_counts_by_inbound_link()
            .dropped_bag_count_snapshot_by_inbound_link(),
        std::collections::BTreeMap::from(
            [("L-first".to_string(), 0), ("L-second".to_string(), 0),]
        ),
    );
    assert_eq!(mailboxes.drain("in").len(), 4);
}
