// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

// =========================================================================
// Naming the inbound link a bag arrived on. The mailbox already knew — the
// per-link drop counter is keyed by it — and these gate that a read hands
// that identity back without disturbing anything it was already doing.
// =========================================================================

/// The read a many-track sink is built on: two producers on one `ordered`
/// port, and every bag comes back naming the link it arrived on.
///
/// Asserted as a set rather than a sequence on purpose. Bags from one link
/// keep that link's order; how two links interleave follows the receive
/// pass, and nothing promises otherwise — a test that pinned the
/// interleaving would be pinning an artifact.
#[test]
fn two_inbound_links_hand_a_reader_the_link_each_bag_arrived_on() {
    let (publisher_a, sub_a) = open_channel_for_one_link("naming/a", 4);
    let (publisher_b, sub_b) = open_channel_for_one_link("naming/b", 4);

    let mailboxes = InputMailboxesInner::new();
    mailboxes.add_port("in", 64, ReadMode::ReadNextInOrder);
    mailboxes.add_channel_subscriber(
        "in",
        "L-camera",
        &InboundLinkName::from("pcamera/video_out"),
        sub_a,
    );
    mailboxes.add_channel_subscriber(
        "in",
        "L-microphone",
        &InboundLinkName::from("pmicrophone/audio_out"),
        sub_b,
    );

    publish_one_frame(&publisher_a, "video_out", b"from-the-camera");
    publish_one_frame(&publisher_b, "audio_out", b"from-the-microphone");
    publish_one_frame(&publisher_a, "video_out", b"from-the-camera-again");

    let mut named: Vec<(String, String)> = Vec::new();
    while let Some((message, inbound_link_name)) = mailboxes
        .read_raw_from_inbound_link("in")
        .expect("a delivered bag names its link")
    {
        named.push((
            String::from_utf8(message.payload.to_vec()).unwrap(),
            inbound_link_name.as_str().to_string(),
        ));
    }
    named.sort();

    assert_eq!(
        named,
        vec![
            (
                "from-the-camera".to_string(),
                "pcamera/video_out".to_string()
            ),
            (
                "from-the-camera-again".to_string(),
                "pcamera/video_out".to_string()
            ),
            (
                "from-the-microphone".to_string(),
                "pmicrophone/audio_out".to_string()
            ),
        ],
        "each bag must come back named by the source channel its own link \
         subscribed to, never by the link that pushed last",
    );
}

/// The new read is a read, not a second accounting path: the same overrun
/// that charges each link its own losses charges exactly the same ones when
/// the survivors are drained by name.
///
/// Mirrors `each_inbound_link_reports_its_own_losses_at_a_stalled_ordered_port`
/// bag for bag — A×5 then B×5 into a depth-2 mailbox — so a divergence
/// between the two is the naming read having disturbed the counting.
#[test]
fn naming_the_inbound_link_a_bag_arrived_on_leaves_the_per_link_drop_counts_alone() {
    const MAILBOX_DEPTH: usize = 2;
    const FRAMES_PER_LINK: usize = 5;

    let (publisher_a, subscriber_a) = open_channel_for_one_link("naming-counts/a", FRAMES_PER_LINK);
    let (publisher_b, subscriber_b) = open_channel_for_one_link("naming-counts/b", FRAMES_PER_LINK);

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

    let mut delivered = 0;
    while let Some((_, inbound_link_name)) = mailboxes
        .read_raw_from_inbound_link("in")
        .expect("a delivered bag names its link")
    {
        assert_eq!(
            inbound_link_name.as_str(),
            "psecond/out",
            "the survivors of this overrun are the second link's, and each must \
             say so rather than inheriting the name of the link it displaced",
        );
        delivered += 1;
    }

    let counts = mailboxes
        .dropped_bag_counts_by_inbound_link()
        .dropped_bag_count_snapshot_by_inbound_link();
    assert_eq!(
        counts,
        std::collections::BTreeMap::from(
            [("L-first".to_string(), 5), ("L-second".to_string(), 3),]
        ),
        "reading by name must charge exactly what the plain read charges",
    );
    assert_eq!(delivered, MAILBOX_DEPTH);
    assert_eq!(
        counts.values().sum::<u64>() + delivered as u64,
        (FRAMES_PER_LINK * 2) as u64,
        "counted plus delivered must still account for every bag published",
    );
}

/// The typed read, and the one place it diverges from
/// [`InputMailboxes::read_msgpack`]: a drained port is `Ok(None)` here where `read_msgpack`
/// is an error. A destination fanning in N links reads until its port runs
/// dry, and an empty port is that loop ending rather than a failure.
#[test]
fn the_typed_read_deserializes_the_bag_and_a_drained_port_is_not_an_error() {
    #[derive(serde::Serialize, serde::Deserialize, PartialEq, Debug)]
    struct OneTrackBag {
        track: String,
    }

    let (publisher, subscriber) = open_channel_for_one_link("naming/typed", 4);

    let mailboxes = InputMailboxesInner::new();
    mailboxes.add_port("in", 8, ReadMode::ReadNextInOrder);
    mailboxes.add_channel_subscriber(
        "in",
        "L-camera",
        &InboundLinkName::from("pcamera/video_out"),
        subscriber,
    );
    publish_one_frame(
        &publisher,
        "video_out",
        &rmp_serde::to_vec_named(&OneTrackBag {
            track: "front".to_string(),
        })
        .expect("the bag encodes"),
    );

    let (bag, inbound_link_name) = mailboxes
        .read_msgpack_from_inbound_link::<OneTrackBag>("in")
        .expect("a well-formed bag deserializes")
        .expect("one bag was published");
    assert_eq!(
        bag,
        OneTrackBag {
            track: "front".to_string()
        }
    );
    assert_eq!(inbound_link_name.as_str(), "pcamera/video_out");

    assert!(
        mailboxes
            .read_msgpack_from_inbound_link::<OneTrackBag>("in")
            .expect("a drained port is not a failure")
            .is_none(),
        "a drained port must end a sink's read loop rather than raise",
    );
}

#[test]
fn a_port_is_read_by_any_spelling_that_casts_to_its_name() {
    let (publisher, subscriber) = open_channel_for_one_link("naming/cast", 4);

    let mailboxes = InputMailboxesInner::new();
    mailboxes.add_port("videoin", 8, ReadMode::ReadNextInOrder);
    mailboxes.add_channel_subscriber(
        "videoin",
        "L-camera",
        &InboundLinkName::from("pcamera/videoout"),
        subscriber,
    );
    let mailboxes = InputMailboxes::from_inner_arc(Arc::new(mailboxes));

    assert!(mailboxes.has_port("videoIn"));
    assert_eq!(
        mailboxes.inbound_link_names("VideoIn"),
        vec![InboundLinkName::from("pcamera/videoout")]
    );
    assert!(!mailboxes.has_data("videoIn"));

    publish_one_frame(&publisher, "videoout", b"first");
    publish_one_frame(&publisher, "videoout", b"second");
    publish_one_frame(
        &publisher,
        "videoout",
        &rmp_serde::to_vec_named("third").expect("the bag encodes"),
    );

    assert!(mailboxes.has_data("videoIn"));
    let payload = mailboxes
        .read_raw("videoIn")
        .expect("a cast-alike spelling names the port")
        .expect("three bags were published")
        .payload;
    assert_eq!(payload, b"first");
    let (message, inbound_link_name) = mailboxes
        .read_raw_from_inbound_link("VideoIn")
        .expect("a cast-alike spelling names the port")
        .expect("two bags remain");
    assert_eq!(message.payload, b"second");
    assert_eq!(inbound_link_name.as_str(), "pcamera/videoout");
    let (bag, _inbound_link_name) = mailboxes
        .read_msgpack_from_inbound_link::<String>("VIDEOIN")
        .expect("a cast-alike spelling names the port")
        .expect("one bag remains");
    assert_eq!(bag, "third");

    let refusal = mailboxes.read_raw("%%").unwrap_err();
    assert!(
        matches!(refusal, Error::ExposedNameCastsToNothing(_)),
        "a name that casts to nothing names no port; got {refusal}"
    );
}

/// What a sink asks in `setup()` to learn how many tracks it owes. WIRE
/// precedes `setup()`, so by the time it asks, every link is there.
///
/// A port with no link at all answers with none rather than refusing: an
/// unconnected input is a legal graph, and a sink that refuses one should
/// do so in its own words.
#[test]
fn a_port_lists_the_inbound_links_wired_into_it_and_a_port_with_none_lists_none() {
    let (_publisher_a, sub_a) = open_channel_for_one_link("listing/a", 1);
    let (_publisher_b, sub_b) = open_channel_for_one_link("listing/b", 1);
    let (_publisher_c, sub_c) = open_channel_for_one_link("listing/c", 1);

    let mailboxes = InputMailboxesInner::new();
    mailboxes.add_port("tracks", 8, ReadMode::ReadNextInOrder);
    mailboxes.add_port("control", 8, ReadMode::ReadNextInOrder);
    mailboxes.add_channel_subscriber(
        "tracks",
        "L-camera",
        &InboundLinkName::from("pcamera/video_out"),
        sub_a,
    );
    mailboxes.add_channel_subscriber(
        "tracks",
        "L-microphone",
        &InboundLinkName::from("pmicrophone/audio_out"),
        sub_b,
    );
    mailboxes.add_channel_subscriber(
        "control",
        "L-operator",
        &InboundLinkName::from("poperator/commands"),
        sub_c,
    );

    assert_eq!(
        mailboxes
            .inbound_link_names("tracks")
            .iter()
            .map(InboundLinkName::as_str)
            .collect::<Vec<_>>(),
        vec!["pcamera/video_out", "pmicrophone/audio_out"],
        "a port lists its own links in wiring order, and no other port's",
    );
    assert_eq!(
        mailboxes
            .inbound_link_names("control")
            .iter()
            .map(InboundLinkName::as_str)
            .collect::<Vec<_>>(),
        vec!["poperator/commands"],
    );
    assert!(
        mailboxes.inbound_link_names("unconnected").is_empty(),
        "a port nothing feeds lists none rather than refusing",
    );
}

/// A window is cut from bags rather than being one, so no queued entry
/// carries its name — but a windowed port takes exactly one link, so the
/// port answers for it and the read works there too.
#[test]
fn a_windowed_ports_read_names_the_one_link_that_feeds_it() {
    let (publisher, subscriber) = open_channel_for_one_link("naming/windowed", 8);

    let mailboxes = InputMailboxesInner::new();
    mailboxes.add_windowed_port(
        "in",
        ReadMode::ReadNextInOrder,
        a_512_512_contract_at(16_000, 1),
    );
    mailboxes.add_channel_subscriber(
        "in",
        "L-only",
        &InboundLinkName::from("pmicrophone/audio_out"),
        subscriber,
    );

    for block in 0..4u64 {
        publish_one_mono_channels_block(
            &publisher,
            160,
            16_000,
            (block * 160 * 1_000_000_000 / 16_000) as i64,
        );
    }

    let (_window, inbound_link_name) = mailboxes
        .read_raw_from_inbound_link("in")
        .expect("640 samples completes a 512 window")
        .expect("a full window reads out");
    assert_eq!(
        inbound_link_name.as_str(),
        "pmicrophone/audio_out",
        "a window names the one link its bags arrived on",
    );
}

/// A read that took a windowed port's stage before the port was unwired and
/// wired again feeds that stage nothing from the new wiring, so the new link
/// is never charged for a flush of the old wiring's samples and its bags stay
/// queued for the stage installed with it.
///
/// Fail-without-fix: the read pops the rewired port's late block into the
/// stage it took, flushes the 300 samples the old wiring left there, counts
/// them on the replacement link, and the replacement's own stage never sees
/// that block.
#[test]
fn a_read_holding_a_stage_from_before_a_rewire_feeds_it_nothing_from_the_new_wiring() {
    let (first_publisher, first_subscriber) = open_channel_for_one_link("rewire/first", 8);
    let mailboxes = InputMailboxesInner::new();
    mailboxes.add_windowed_port(
        "in",
        ReadMode::ReadNextInOrder,
        a_512_512_contract_at(16_000, 1),
    );
    mailboxes.add_channel_subscriber(
        "in",
        "L-first",
        &InboundLinkName::from("pfirst/audio_out"),
        first_subscriber,
    );
    publish_one_mono_channels_block(&first_publisher, 300, 16_000, 0);
    assert!(
        mailboxes
            .read_raw("in")
            .expect("the read succeeds")
            .is_none(),
        "300 of 512 samples is not a window"
    );
    let stage_the_read_took = match &mailboxes.ports.lock()["in"].audio_windowing {
        InstalledInputPortAudioWindowing::Windowed(stage) => Arc::clone(stage),
        _ => panic!("the port windows"),
    };

    mailboxes.remove_channel_link("L-first");
    let (second_publisher, second_subscriber) = open_channel_for_one_link("rewire/second", 8);
    mailboxes.add_windowed_port(
        "in",
        ReadMode::ReadNextInOrder,
        a_512_512_contract_at(16_000, 1),
    );
    mailboxes.add_channel_subscriber(
        "in",
        "L-second",
        &InboundLinkName::from("psecond/audio_out"),
        second_subscriber,
    );
    let a_second_later_ns = 1_000_000_000;
    publish_one_mono_channels_block(&second_publisher, 300, 16_000, a_second_later_ns);
    mailboxes.receive_pending();

    assert!(
        mailboxes
            .next_window_out_of_the_stage("in", &stage_the_read_took)
            .expect("the read succeeds")
            .is_none(),
        "the stage the read took is no longer the port's"
    );
    let discarded_samples_by_link = || {
        mailboxes
            .discarded_sample_counts_by_inbound_link()
            .discarded_sample_count_snapshot_by_inbound_link()
    };
    assert_eq!(
        discarded_samples_by_link(),
        std::collections::BTreeMap::from([("L-second".to_string(), 0)]),
    );

    publish_one_mono_channels_block(
        &second_publisher,
        300,
        16_000,
        a_second_later_ns + 18_750_000,
    );
    assert!(
        mailboxes
            .read_raw("in")
            .expect("the read succeeds")
            .is_some(),
        "both of the new wiring's blocks reach its own stage and make a window"
    );
    assert_eq!(
        discarded_samples_by_link(),
        std::collections::BTreeMap::from([("L-second".to_string(), 0)]),
    );
}
