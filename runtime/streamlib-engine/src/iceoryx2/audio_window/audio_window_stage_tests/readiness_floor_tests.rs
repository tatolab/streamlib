// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The readiness floor a read is gated on, which never claims a window the read
//! cannot then produce.

use super::*;

fn assert_the_readiness_floor_holds_for(
    contract: ResolvedAudioWindowContract,
    source: SourceAudioFormat,
    channels_each_window_carries: u32,
) {
    let SourceAudioFormat {
        sample_rate: source_rate,
        channels: source_channels,
    } = source;
    let (mut stage, format_the_mailbox_reports) =
        stage_and_the_format_its_mailbox_reports(contract);

    let source_frames_per_block = 160u64;
    let mut queued_equivalents = 0u64;
    let mut blocks = Vec::new();
    let mut the_floor_cleared = false;
    for block_index in 0..60u64 {
        let first_frame = block_index * source_frames_per_block;
        blocks.push(source_block(
            &interleaved_sine(
                first_frame,
                source_frames_per_block as usize,
                source_channels,
                source_rate,
                440.0,
            ),
            source_rate,
            source_channels,
            nanoseconds_for(first_frame, source_rate),
        ));
        queued_equivalents +=
            source_frames_per_block * u64::from(contract.sample_rate) / u64::from(source_rate);
        format_the_mailbox_reports.record(source);

        if !stage.a_full_window_would_be_ready_after(queued_equivalents, false) {
            continue;
        }
        // The gate said yes, so feeding exactly what was queued must produce a
        // window.
        for block in blocks.drain(..) {
            stage.accept(&block).expect("accepted");
        }
        queued_equivalents = 0;
        let window = stage
            .next_ready_window()
            .expect("a window emits")
            .unwrap_or_else(|| {
                panic!(
                    "the readiness floor claimed a window at {source_rate} Hz / \
                     {source_channels} channels that the read could not produce"
                )
            });
        let emitted = channels_block_of_the_window(&window.message);
        assert_eq!(
            emitted.channels, channels_each_window_carries,
            "the window the floor promised carries the count the contract resolves to"
        );
        the_floor_cleared = true;
    }

    assert!(
        the_floor_cleared,
        "the floor never cleared at {source_rate} Hz across {source_channels} channels, so \
         it never claimed anything to check"
    );
}

/// The readiness floor must never claim a window the read then cannot produce:
/// a reactive `process()` that woke and found nothing is the shape the window
/// contract exists to rule out.
#[test]
fn the_readiness_floor_never_claims_a_window_the_read_cannot_then_produce() {
    for (source_rate, source_channels) in [(48_000u32, 2u32), (16_000, 1), (44_100, 1)] {
        assert_the_readiness_floor_holds_for(
            contract(16_000, 1, "f32", 512, 512),
            SourceAudioFormat {
                sample_rate: source_rate,
                channels: source_channels,
            },
            1,
        );
    }
}

/// The floor's one bounded exception, pinned so it stays bounded: the measure
/// counts a queued bag's samples and never reads its stamp, so a discontinuity
/// sitting in the queue is invisible to it and the read that follows flushes
/// what it had accumulated. That costs exactly one empty read, and the very
/// next window arrives from the run the gap started.
#[test]
fn a_gap_hidden_in_the_queue_costs_one_empty_read_and_no_more() {
    let contract = contract(16_000, 1, "f32", 512, 512);
    let (mut stage, format_the_mailbox_reports) =
        stage_and_the_format_its_mailbox_reports(contract);
    format_the_mailbox_reports.record(SourceAudioFormat {
        sample_rate: 16_000,
        channels: 1,
    });

    // Three 160-sample bags — 480 of the 512 a window needs.
    let mut queued: Vec<MessageReadFromInputPort> = (0..3u64)
        .map(|block| {
            source_block(
                &interleaved_sine(block * 160, 160, 1, 16_000, 300.0),
                16_000,
                1,
                nanoseconds_for(block * 160, 16_000),
            )
        })
        .collect();
    assert!(
        !stage.a_full_window_would_be_ready_after(480, false),
        "480 of 512 samples is not a window"
    );

    // A fourth bag a whole second later takes the queue past 512, and the gate
    // — which cannot see its stamp — says yes.
    queued.push(source_block(
        &interleaved_sine(0, 160, 1, 16_000, 300.0),
        16_000,
        1,
        NANOSECONDS_PER_SECOND,
    ));
    assert!(stage.a_full_window_would_be_ready_after(640, false));

    for block in queued {
        stage.accept(&block).expect("accepted");
    }
    assert!(
        stage.next_ready_window().expect("asked").is_none(),
        "the gap flushed the 480 samples before it, so there is genuinely no window \
         to hand over — this is the one empty read"
    );

    // And the cost stops there: the run the gap started fills a window of its
    // own, and the gate is right about it.
    let mut queued_after_the_gap = 160u64;
    for block in 1..4u64 {
        stage
            .accept(&source_block(
                &interleaved_sine(block * 160, 160, 1, 16_000, 300.0),
                16_000,
                1,
                NANOSECONDS_PER_SECOND + nanoseconds_for(block * 160, 16_000),
            ))
            .expect("accepted");
        queued_after_the_gap += 160;
    }
    assert!(stage.a_full_window_would_be_ready_after(0, false));
    assert_eq!(queued_after_the_gap, 640);
    let window = stage
        .next_ready_window()
        .expect("a window emits")
        .expect("the run after the gap fills its own window");
    let block = channels_block_of_the_window(&window.message);
    assert_eq!(
        block.first_sample_timestamp_ns, NANOSECONDS_PER_SECOND,
        "the window after the gap is anchored on its own run's first block"
    );
}

/// A port whose source publishes quanta far smaller than the depth assumed can
/// fill its mailbox without ever filling a window. It delivers nothing and
/// evicts everything behind it, and the only other signal is a drop counter
/// climbing — so it says so, naming the port, and says it once.
#[test]
fn a_full_mailbox_that_still_cannot_make_a_window_says_so_once() {
    let contract = contract(16_000, 1, "f32", 512, 512);
    let (mut stage, format_the_mailbox_reports) =
        stage_and_the_format_its_mailbox_reports(contract);
    format_the_mailbox_reports.record(SourceAudioFormat {
        sample_rate: 16_000,
        channels: 1,
    });

    assert!(!stage.has_said_a_full_mailbox_cannot_fill_a_window());

    // Short of a window, but the mailbox still has room: that is an ordinary
    // wait, not a stall.
    assert!(!stage.a_full_window_would_be_ready_after(100, false));
    assert!(
        !stage.has_said_a_full_mailbox_cannot_fill_a_window(),
        "a port with room left is waiting, not stalled"
    );

    // A full mailbox worth only 100 of the 512 samples a window needs is a port
    // that will never deliver anything.
    assert!(!stage.a_full_window_would_be_ready_after(100, true));
    assert!(stage.has_said_a_full_mailbox_cannot_fill_a_window());

    // And a port that recovers stops being described as stalled.
    assert!(stage.a_full_window_would_be_ready_after(600, true));
}

#[test]
fn the_readiness_floor_says_yes_well_inside_the_depth_the_mailbox_is_sized_to() {
    let contract = contract(16_000, 1, "f32", 16_000, 160);
    let (mut stage, format_the_mailbox_reports) =
        stage_and_the_format_its_mailbox_reports(contract);
    format_the_mailbox_reports.record(SourceAudioFormat {
        sample_rate: 48_000,
        channels: 1,
    });

    let depth = contract.windowed_port_mailbox_depth() as u64;
    let mut queued_equivalents = 0u64;
    let mut said_yes_after = None;
    for queued_blocks in 1..=depth {
        queued_equivalents += 512 * u64::from(contract.sample_rate) / 48_000;
        if stage.a_full_window_would_be_ready_after(queued_equivalents, false) {
            said_yes_after = Some(queued_blocks);
            break;
        }
    }

    let said_yes_after =
        said_yes_after.expect("the floor must clear inside the depth the mailbox holds");
    assert!(
        said_yes_after <= depth,
        "the gate cleared only after {said_yes_after} blocks, past the {depth} the mailbox holds"
    );
}

/// The readiness floor is asked before any bag has been consumed, so on a
/// contract that declares no count it must answer from the count the mailbox's
/// measure saw — and the windows it promises must carry that count.
#[test]
fn readiness_on_a_channel_free_contract_never_claims_a_window_the_read_cannot_produce() {
    for (source_rate, source_channels) in [(48_000u32, 2u32), (16_000, 1), (44_100, 6)] {
        assert_the_readiness_floor_holds_for(
            contract_following_the_sources_channels(16_000, "f32", 512, 512),
            SourceAudioFormat {
                sample_rate: source_rate,
                channels: source_channels,
            },
            source_channels,
        );
    }
}
