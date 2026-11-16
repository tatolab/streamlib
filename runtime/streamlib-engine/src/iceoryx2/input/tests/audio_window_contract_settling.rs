// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use crate::core::test_support::CapturedTracingWarnings;

// =========================================================================
// The audio window contract at the read seam, through real iceoryx2
// services. These need /dev/shm and nothing else — no GPU, no audio device.
// =========================================================================

/// A 48 kHz source into a 16 kHz port takes the resampling arm, where the
/// readiness floor has to know the source rate to give back the right
/// priming — and the only place it can learn one before consuming a bag is
/// the mailbox's own measure.
///
/// The identity arm cannot catch a measure that reports no rate: with the
/// rates equal there is no filter and no priming to give back.
#[test]
fn a_resampling_windowed_port_reports_data_only_once_a_full_window_can_be_emitted() {
    let (publisher, subscriber) = open_channel_for_one_link_loaning("window/resampled", 32, 16_384);

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

    // 160-frame quanta, so the queue crosses one window's worth in steps
    // smaller than the priming and chunk slack. A floor that cannot see
    // that slack says yes somewhere in that gap and the read then produces
    // nothing — the exact shape the contract exists to rule out, and one a
    // coarser quantum steps straight over.
    const SOURCE_FRAMES_PER_BLOCK: u64 = 160;

    let mut windows = 0;
    for block in 0..24u64 {
        publish_one_mono_channels_block(
            &publisher,
            SOURCE_FRAMES_PER_BLOCK as usize,
            48_000,
            (block * SOURCE_FRAMES_PER_BLOCK * 1_000_000_000 / 48_000) as i64,
        );
        while mailboxes.any_port_has_data() {
            assert!(
                mailboxes.read_raw("in").expect("reads").is_some(),
                "the gate promised a window at 48 kHz into a 16 kHz port; the read \
                 must produce one"
            );
            windows += 1;
        }
    }

    assert!(
        windows >= 2,
        "24 blocks of 160 frames at 48 kHz is 1280 frames at 16 kHz, so at least \
         two 512-sample windows; got {windows}"
    );
}

fn a_device_stream_matching(
    sample_rate: u32,
    channels: u32,
    window_and_hop: u32,
) -> AudioWindowContractMatchingADeviceStream {
    AudioWindowContractMatchingADeviceStream {
        device_stream_format: crate::core::context::AudioStreamFormat {
            sample_rate,
            channels,
            sample_format: crate::core::context::AudioSampleFormat::F32,
        },
        window_size_in_per_channel_samples: window_and_hop,
        hop_in_per_channel_samples: window_and_hop,
    }
}

/// The samples one emitted window carries, interleaved, with its rate and
/// channel count, read back out of the Channels block the stage wrote.
fn scalars_of_one_window(window: &MessageReadFromInputPort) -> (Vec<f32>, u32, u32) {
    let block = crate::iceoryx2::audio_window::read_a_channels_block_for_the_window_stage(
        &window.payload_descriptor,
        &window.payload,
        window.timestamp_ns,
    )
    .expect("the stage writes a Channels block");
    (
        block.interleaved_samples_as_f32().collect(),
        block.sample_rate,
        block.channels,
    )
}

#[test]
fn a_port_awaiting_its_device_hands_a_reader_nothing_however_much_is_queued() {
    let (publisher, subscriber) = open_channel_for_one_link_loaning("window/awaiting", 8, 16_384);

    let mailboxes = InputMailboxesInner::new();
    mailboxes.add_port_awaiting_its_device_stream_format(
        "in",
        crate::iceoryx2::DeliveryProfile::ORDERED_DEPTH,
        ReadMode::ReadNextInOrder,
    );
    mailboxes.add_channel_subscriber(
        "in",
        "L-only",
        &InboundLinkName::from("ponly/out"),
        subscriber,
    );

    for block in 0..4u64 {
        publish_one_mono_channels_block(&publisher, 480, 16_000, (block * 30_000_000) as i64);
    }

    assert!(
        !mailboxes.has_data("in"),
        "a port with no settled contract has no exact size to cut a window to"
    );
    assert!(!mailboxes.any_port_has_data());
    assert!(
        mailboxes
            .read_raw("in")
            .expect("reading an unsettled port is not an error")
            .is_none(),
        "the raw bag underneath is exactly what the contract exists to keep out of \
         process()"
    );
    assert_eq!(
        mailboxes.input_ports_still_awaiting_their_device_stream_format(),
        vec!["in".to_string()]
    );
}

#[test]
fn a_bag_evicted_at_a_port_with_an_unsettled_contract_says_so_once_naming_the_port() {
    const MAILBOX_DEPTH: usize = 2;
    const FRAMES_PUBLISHED: usize = 6;

    let (publisher, subscriber) = open_channel_for_one_link("window/awaiting-drop", 4);

    let mailboxes = InputMailboxesInner::new();
    mailboxes.add_port_awaiting_its_device_stream_format(
        "in",
        MAILBOX_DEPTH,
        ReadMode::ReadNextInOrder,
    );
    mailboxes.add_channel_subscriber(
        "in",
        "L-only",
        &InboundLinkName::from("ponly/out"),
        subscriber,
    );

    let ((), warnings) = CapturedTracingWarnings::captured_while(|| {
        for _ in 0..FRAMES_PUBLISHED {
            publish_one_frame(&publisher, "mic_out", b"before-the-device-spoke");
            mailboxes.receive_pending();
        }
    });

    assert_eq!(
        mailboxes
            .dropped_bag_counts_by_inbound_link()
            .dropped_bag_count_snapshot_by_inbound_link()
            .get("L-only")
            .copied(),
        Some((FRAMES_PUBLISHED - MAILBOX_DEPTH) as u64),
        "the loss is still counted against the link that lost it"
    );
    let about_the_unsettled_contract: Vec<&String> = warnings
        .iter()
        .filter(|said| said.contains("contract is unsettled"))
        .collect();
    assert_eq!(
        about_the_unsettled_contract.len(),
        1,
        "four bags were evicted and the port diagnoses the run once, not once per \
         bag; got {warnings:?}"
    );
    let said = about_the_unsettled_contract[0];
    assert!(
        said.contains("port=in") && said.contains("match_device"),
        "the record must name the port and the contract that never settled; got {said}"
    );
}

/// The notice belongs to the mailbox the sentinel holds, not to the port:
/// once the contract settles the port is sized from it like any other, and
/// an overrun there is an ordinary one that must read as one.
#[test]
fn a_settled_ports_evictions_are_not_reported_as_an_unsettled_contract() {
    const FRAMES_PUBLISHED: usize = 24;

    let (publisher, subscriber) = open_channel_for_one_link("window/settled-drop", 4);

    let mailboxes = InputMailboxesInner::new();
    mailboxes.add_port_awaiting_its_device_stream_format(
        "in",
        crate::iceoryx2::DeliveryProfile::ORDERED_DEPTH,
        ReadMode::ReadNextInOrder,
    );
    mailboxes.add_channel_subscriber(
        "in",
        "L-only",
        &InboundLinkName::from("ponly/out"),
        subscriber,
    );
    mailboxes
        .settle_a_ports_device_matched_audio_window_contract(
            "in",
            &a_device_stream_matching(48_000, 2, 48),
        )
        .expect("a device format settles the contract");

    let ((), warnings) = CapturedTracingWarnings::captured_while(|| {
        for _ in 0..FRAMES_PUBLISHED {
            publish_one_frame(&publisher, "mic_out", b"after-the-device-spoke");
            mailboxes.receive_pending();
        }
    });

    assert!(
        mailboxes
            .dropped_bag_counts_by_inbound_link()
            .dropped_bag_count_snapshot_by_inbound_link()
            .get("L-only")
            .copied()
            .unwrap_or(0)
            > 0,
        "the run has to overrun the settled depth, or it proves nothing"
    );
    assert!(
        !warnings
            .iter()
            .any(|said| said.contains("contract is unsettled")),
        "a settled port's overrun is an ordinary one; got {warnings:?}"
    );
}

/// The rung's flagship case, proven without a device: a mono-preferring
/// microphone's blocks are already queued when the speaker's `setup()`
/// settles its port from a stereo-preferring playback stream, and they come
/// back out converted rather than refused.
///
/// Held against a control rather than against a number: the same bags into
/// a port that windowed from the start must yield the same windows. That is
/// what the swap owes — the mailbox it replaces is a different queue, and a
/// bag dropped in the move would be lost where nothing counts it, because
/// the link it arrived on is still wired and still delivering. A count
/// asserted on its own would also pass on two empty runs, so the control
/// carries a floor.
#[test]
fn settling_a_port_converts_the_bags_that_arrived_before_the_device_was_known() {
    /// Publish four 16 kHz mono blocks onto port `"in"` and draw them into
    /// its mailbox, so what follows has something queued to work on.
    fn publish_four_mono_blocks_into(mailboxes: &InputMailboxesInner, tag: &str) {
        let (publisher, subscriber) = open_channel_for_one_link_loaning(tag, 16, 16_384);
        mailboxes.add_channel_subscriber(
            "in",
            "L-only",
            &InboundLinkName::from("ponly/out"),
            subscriber,
        );
        for block in 0..4u64 {
            publish_one_mono_channels_block(&publisher, 480, 16_000, (block * 30_000_000) as i64);
        }
        mailboxes.receive_pending();
    }

    /// Every window port `"in"` hands back, each held to the device format
    /// its contract settled to.
    fn windows_read_out_of(mailboxes: &InputMailboxesInner) -> Vec<Vec<f32>> {
        let mut windows = Vec::new();
        while let Some(message) = mailboxes.read_raw("in").expect("the port reads") {
            let (scalars, sample_rate, channels) = scalars_of_one_window(&message);
            assert_eq!(sample_rate, 48_000, "the window is at the device's rate");
            assert_eq!(channels, 2, "the window is in the device's channel count");
            assert_eq!(
                scalars.len(),
                480 * 2,
                "a window carries window_size × channels scalars"
            );
            windows.push(scalars);
        }
        windows
    }

    let matching = a_device_stream_matching(48_000, 2, 480);

    let settled_after_the_bags_arrived = InputMailboxesInner::new();
    settled_after_the_bags_arrived.add_port_awaiting_its_device_stream_format(
        "in",
        crate::iceoryx2::DeliveryProfile::ORDERED_DEPTH,
        ReadMode::ReadNextInOrder,
    );
    publish_four_mono_blocks_into(&settled_after_the_bags_arrived, "window/settled");
    settled_after_the_bags_arrived
        .settle_a_ports_device_matched_audio_window_contract("in", &matching)
        .expect("a playback stream's own format settles the contract");
    assert!(
        settled_after_the_bags_arrived
            .input_ports_still_awaiting_their_device_stream_format()
            .is_empty(),
        "a settled port is no longer awaiting anything"
    );
    let across_the_settle = windows_read_out_of(&settled_after_the_bags_arrived);

    let windowed_from_the_start = InputMailboxesInner::new();
    windowed_from_the_start.add_windowed_port(
        "in",
        ReadMode::ReadNextInOrder,
        ResolvedAudioWindowContract::from_a_device_stream_format(&matching)
            .expect("the same contract, stated up front"),
    );
    publish_four_mono_blocks_into(&windowed_from_the_start, "window/control");
    let never_awaited = windows_read_out_of(&windowed_from_the_start);

    assert!(
        never_awaited.len() >= 8,
        "1 920 mono frames at 16 kHz are 5 760 at 48 kHz, so a port that windowed them \
         all along must hand back most of twelve 480-sample windows; got {}",
        never_awaited.len()
    );
    assert_eq!(
        across_the_settle, never_awaited,
        "settling a port after its bags arrived must lose none of them and change none \
         of their samples — the swap moved the queue, it did not re-cut the audio"
    );
}

/// A port with no contract is untouched by any of this, and a settled one
/// stops being listed the moment it settles — the two answers the
/// post-`setup()` refusal is built on.
#[test]
fn only_a_port_still_holding_the_sentinel_is_listed_as_awaiting_its_device() {
    let mailboxes = InputMailboxesInner::new();
    mailboxes.add_port("plain", 4, ReadMode::ReadNextInOrder);
    mailboxes.add_windowed_port(
        "declared",
        ReadMode::ReadNextInOrder,
        a_512_512_contract_at(16_000, 1),
    );
    mailboxes.add_port_awaiting_its_device_stream_format(
        "matched",
        crate::iceoryx2::DeliveryProfile::ORDERED_DEPTH,
        ReadMode::ReadNextInOrder,
    );

    assert_eq!(
        mailboxes.input_ports_still_awaiting_their_device_stream_format(),
        vec!["matched".to_string()],
        "neither a contract-less port nor one stating five values is waiting on a device"
    );

    mailboxes
        .settle_a_ports_device_matched_audio_window_contract(
            "matched",
            &a_device_stream_matching(48_000, 2, 480),
        )
        .expect("the contract settles");

    assert!(
        mailboxes
            .input_ports_still_awaiting_their_device_stream_format()
            .is_empty()
    );
}

#[test]
fn only_a_port_awaiting_its_device_can_be_settled_and_the_refusal_says_which_it_is() {
    let mailboxes = InputMailboxesInner::new();
    mailboxes.add_port("plain", 4, ReadMode::ReadNextInOrder);
    mailboxes.add_windowed_port(
        "declared",
        ReadMode::ReadNextInOrder,
        a_512_512_contract_at(16_000, 1),
    );
    mailboxes.add_port_awaiting_its_device_stream_format(
        "matched",
        crate::iceoryx2::DeliveryProfile::ORDERED_DEPTH,
        ReadMode::ReadNextInOrder,
    );
    let matching = a_device_stream_matching(48_000, 2, 480);

    for (port, expected) in [
        ("plain", "declares no window contract"),
        ("declared", "already settled"),
    ] {
        let refusal = mailboxes
            .settle_a_ports_device_matched_audio_window_contract(port, &matching)
            .expect_err("only a waiting port is settled")
            .to_string();
        assert!(
            refusal.contains(port) && refusal.contains(expected),
            "the refusal must name the port and what it is; got {refusal}"
        );
    }

    mailboxes
        .settle_a_ports_device_matched_audio_window_contract("matched", &matching)
        .expect("the waiting port settles");
    let settled_twice = mailboxes
        .settle_a_ports_device_matched_audio_window_contract("matched", &matching)
        .expect_err("a settled port is not settled again")
        .to_string();
    assert!(
        settled_twice.contains("already settled"),
        "a second settle must say the contract is already settled; got {settled_twice}"
    );
}

/// A link wired after `setup()` already ran — a live-graph edit — finds the
/// settled values rather than a sentinel, so its port windows from the
/// first bag instead of waiting for a `setup()` that has been and gone.
#[test]
fn a_contract_settled_before_its_port_existed_is_still_there_for_the_wiring() {
    let mailboxes = InputMailboxesInner::new();
    mailboxes
        .settle_a_ports_device_matched_audio_window_contract(
            "in",
            &a_device_stream_matching(44_100, 2, 441),
        )
        .expect("a contract settles whether or not the port is wired yet");

    let settled = mailboxes
        .device_matched_audio_window_contracts()
        .settled_declaration_for_input_port("in")
        .expect("the wiring path finds what setup() settled");
    assert_eq!(settled.sample_rate, 44_100);
    assert_eq!(settled.channels, Some(2));
    assert_eq!(settled.window_size, 441);
    assert_eq!(settled.hop, 441);
}

/// A device stream the stage could not honour is refused where it is
/// settled, naming the port — never accepted and left to fail at the first
/// read.
#[test]
fn a_device_format_the_stage_cannot_honour_is_refused_at_the_settle_naming_the_port() {
    let mailboxes = InputMailboxesInner::new();
    mailboxes.add_port_awaiting_its_device_stream_format(
        "in",
        crate::iceoryx2::DeliveryProfile::ORDERED_DEPTH,
        ReadMode::ReadNextInOrder,
    );

    let refusal = mailboxes
        .settle_a_ports_device_matched_audio_window_contract(
            "in",
            &a_device_stream_matching(48_000, 2, 0),
        )
        .expect_err("a zero window is not a contract")
        .to_string();

    assert!(
        refusal.contains("'in'") && refusal.contains("window_size"),
        "the refusal must name the port and the field; got {refusal}"
    );
    assert_eq!(
        mailboxes.input_ports_still_awaiting_their_device_stream_format(),
        vec!["in".to_string()],
        "a refused settle leaves the port where it was rather than half-settled"
    );
}
