// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A gap or format change flushing the filter and the remainder, and what each
//! flush counts as discarded.

use super::*;

/// The assertion that catches a missed filter reset: a polyphase resampler
/// holds a filter's length of pre-gap samples, so a stage that flushed its
/// accumulator but not its filter would carry loud pre-gap audio into the
/// silence after the gap.
#[test]
fn the_first_window_after_a_gap_carries_no_energy_from_before_it() {
    let contract = contract(16_000, 1, "f32", 512, 512);
    let mut stage = stage_on(contract);

    // A full-scale tone, right up to the gap.
    for block_index in 0..24u64 {
        let first_frame = block_index * 512;
        stage
            .accept(&source_block(
                &interleaved_sine(first_frame, 512, 1, 48_000, 1_000.0),
                48_000,
                1,
                nanoseconds_for(first_frame, 48_000),
            ))
            .expect("accepted");
        drain_every_ready_window(&mut stage);
    }

    // A second of nothing, then silence resumes. The stamp is a whole second
    // past where the previous block ended — far outside half a quantum.
    let after_the_gap_ns = nanoseconds_for(24 * 512, 48_000) + NANOSECONDS_PER_SECOND;
    let mut windows = Vec::new();
    for block_index in 0..24u64 {
        let first_frame = block_index * 512;
        stage
            .accept(&source_block(
                &vec![0.0f32; 512],
                48_000,
                1,
                after_the_gap_ns + nanoseconds_for(first_frame, 48_000),
            ))
            .expect("accepted");
        windows.extend(drain_every_ready_window(&mut stage));
    }

    let first_after_the_gap = windows.first().expect("a window emitted after the gap");
    let loudest = first_after_the_gap
        .scalars
        .iter()
        .fold(0.0f32, |loudest, sample| loudest.max(sample.abs()));
    assert!(
        loudest < 1e-6,
        "the first post-gap window must hold only the silence that followed the gap; \
         its loudest sample is {loudest}, which is pre-gap tone leaking through a filter \
         the flush did not reset"
    );
    assert_eq!(
        first_after_the_gap.first_sample_timestamp_ns, after_the_gap_ns,
        "the window after the gap is anchored on its own block's stamp"
    );
}

/// No window spans the gap: every stamp is consistent with its own run, so a
/// consumer joining by timestamp never sees a block whose samples straddle the
/// loss.
#[test]
fn no_window_spans_a_gap_in_the_source_stream() {
    let contract = contract(16_000, 1, "f32", 512, 512);
    let mut stage = stage_on(contract);

    let gap_ns = NANOSECONDS_PER_SECOND / 2;
    let mut windows = Vec::new();
    for block_index in 0..20u64 {
        let first_frame = block_index * 512;
        // The gap opens after the tenth block and never closes back up.
        let displaced = if block_index >= 10 { gap_ns } else { 0 };
        stage
            .accept(&source_block(
                &interleaved_sine(first_frame, 512, 1, 16_000, 300.0),
                16_000,
                1,
                nanoseconds_for(first_frame, 16_000) + displaced,
            ))
            .expect("accepted");
        windows.extend(drain_every_ready_window(&mut stage));
    }

    let gap_start_ns = nanoseconds_for(10 * 512, 16_000);
    for window in &windows {
        let window_end_ns = window.first_sample_timestamp_ns
            + nanoseconds_for(u64::from(window.sample_count), 16_000);
        let spans_the_gap = window.first_sample_timestamp_ns < gap_start_ns
            && window_end_ns > gap_start_ns + gap_ns;
        assert!(
            !spans_the_gap,
            "a window from {} to {window_end_ns} spans the gap the flush exists to break",
            window.first_sample_timestamp_ns
        );
    }
}

/// The tightest gap there is, and the one the mailbox itself creates: a single
/// evicted bag. The change file says a bag evicted at a windowed port costs its
/// own samples plus the flush of the remainder behind it — and nothing hooks
/// the eviction to do that, because the displacement the eviction leaves in the
/// stamps is what the flush is driven by. One quantum of displacement against a
/// half-quantum tolerance is the narrowest case that must still trip it.
#[test]
fn a_single_evicted_block_displaces_the_stamps_enough_to_flush() {
    let contract = contract(16_000, 1, "f32", 512, 512);
    let mut stage = stage_on(contract);

    let mut windows = Vec::new();
    for block_index in 0..8u64 {
        // Block 4 never arrives — the mailbox evicted it under overrun.
        if block_index == 4 {
            continue;
        }
        let first_frame = block_index * 512;
        stage
            .accept(&source_block(
                &interleaved_sine(first_frame, 512, 1, 16_000, 300.0),
                16_000,
                1,
                nanoseconds_for(first_frame, 16_000),
            ))
            .expect("accepted");
        windows.extend(drain_every_ready_window(&mut stage));
    }

    // Seven blocks of 512 arrived, so eight windows would mean a window was
    // built across the hole.
    assert_eq!(
        windows.len(),
        7,
        "each arriving block fills one window, and the evicted one fills none"
    );
    let stamp_of_the_evicted_block = nanoseconds_for(4 * 512, 16_000);
    assert!(
        !windows
            .iter()
            .any(|window| window.first_sample_timestamp_ns == stamp_of_the_evicted_block),
        "no window may claim the instant the evicted block covered"
    );
    // The run re-anchors after the hole rather than carrying the old anchor
    // across it, so the stamps step by a window everywhere except across it.
    let steps: Vec<i64> = windows
        .windows(2)
        .map(|pair| pair[1].first_sample_timestamp_ns - pair[0].first_sample_timestamp_ns)
        .collect();
    assert_eq!(
        steps.iter().filter(|step| **step != 32_000_000).count(),
        1,
        "exactly one step spans the hole; the rest are one window apart — got {steps:?}"
    );
}

/// A stamp jittering inside half a source quantum is the device being exact to
/// the block rather than to the sample, and must not flush a healthy run.
///
/// The jitter is bounded relative to the *previous* stamp, because that is what
/// the tolerance is measured against: a block's expected position is the last
/// block's stamp plus its duration, so two consecutive stamps displaced in
/// opposite directions is a relative jump, not jitter.
#[test]
fn a_stamp_jittering_inside_half_a_quantum_does_not_flush_the_run() {
    let contract = contract(16_000, 1, "f32", 512, 512);
    let mut stage = stage_on(contract);

    let an_eighth_of_a_quantum_ns = nanoseconds_for(512, 16_000) / 8;
    let mut windows = Vec::new();
    for block_index in 0..8u64 {
        let first_frame = block_index * 512;
        let jitter = (block_index as i64 % 3 - 1) * an_eighth_of_a_quantum_ns;
        stage
            .accept(&source_block(
                &interleaved_sine(first_frame, 512, 1, 16_000, 300.0),
                16_000,
                1,
                nanoseconds_for(first_frame, 16_000) + jitter,
            ))
            .expect("accepted");
        windows.extend(drain_every_ready_window(&mut stage));
    }

    assert_eq!(
        windows.len(),
        8,
        "eight 512-sample blocks satisfy eight 512-sample windows; a flush would have \
         discarded some"
    );
    for pair in windows.windows(2) {
        assert_eq!(
            pair[1].first_sample_timestamp_ns - pair[0].first_sample_timestamp_ns,
            32_000_000,
            "the run stayed contiguous, so every stamp derived from the one anchor and \
             the stamps stayed exactly one window apart — jitter reaches the anchor only \
             when a flush re-anchors"
        );
    }
}

/// A gap flush counts what it discarded in the samples the consumer would have
/// received: per-channel samples at the port's declared rate.
///
/// A hundred stereo frames at 48 kHz sit staged, short of one resampler chunk,
/// when the gap arrives. At the declared 16 kHz they were worth 33⅓ samples, so
/// the flush counts 33. Counting the source's frames gives 100, its interleaved
/// scalars 200, and rounding up 34.
#[test]
fn a_gap_flush_counts_its_staged_source_frames_as_samples_at_the_declared_rate() {
    let mut stage = stage_on(contract(16_000, 1, "f32", 512, 512));

    let flush_before_the_gap = stage
        .accept(&source_block(
            &interleaved_sine(0, 100, 2, 48_000, 440.0),
            48_000,
            2,
            0,
        ))
        .expect("accepted");
    assert!(
        flush_before_the_gap.is_none(),
        "the first block of a run flushes nothing"
    );

    let flush = stage
        .accept(&source_block(
            &interleaved_sine(0, 100, 2, 48_000, 440.0),
            48_000,
            2,
            NANOSECONDS_PER_SECOND,
        ))
        .expect("accepted")
        .expect("a block a second past where the last one ended flushes");

    assert_eq!(
        flush.cause,
        AudioWindowStageFlushCause::BlockArrivedAwayFromWhereThePreviousEnded
    );
    assert_eq!(flush.discarded_per_channel_samples_at_the_declared_rate, 33);
}

/// A format-change flush counts the output remainder per channel, never per
/// interleaved scalar.
///
/// Three hundred stereo frames already at the contract's rate pass straight into
/// the remainder, short of a 512-sample window, when the source drops to mono.
/// The flush counts 300; counting the scalars the remainder holds gives 600.
#[test]
fn a_format_change_flush_counts_the_remainder_in_per_channel_samples() {
    let mut stage = stage_on(contract_following_the_sources_channels(
        16_000, "f32", 512, 512,
    ));

    stage
        .accept(&source_block(
            &interleaved_sine(0, 300, 2, 16_000, 440.0),
            16_000,
            2,
            0,
        ))
        .expect("accepted");

    let flush = stage
        .accept(&source_block(
            &interleaved_sine(300, 100, 1, 16_000, 440.0),
            16_000,
            1,
            nanoseconds_for(300, 16_000),
        ))
        .expect("accepted")
        .expect("a block in another channel count flushes, however contiguous its stamp");

    assert_eq!(
        flush.cause,
        AudioWindowStageFlushCause::SourceChangedFormatMidStream
    );
    assert_eq!(
        flush.discarded_per_channel_samples_at_the_declared_rate,
        300
    );
}

/// A bag the stage refuses flushes nothing, so the remainder a later flush
/// discards is still there to be counted.
///
/// Three hundred four-channel frames sit in the remainder when four hundred
/// stereo frames, which a four-channel contract cannot convert, arrive and are
/// refused. The next four-channel block lands where the refused one ended, well
/// past half a block from where the remainder's run expects, and its flush
/// counts the 300. Flushing before the refusal throws those 300 away with no
/// count and no warning.
#[test]
fn a_refused_block_flushes_nothing_and_the_next_flush_counts_what_it_left() {
    let mut stage = stage_on(contract(16_000, 4, "f32", 512, 512));

    stage
        .accept(&source_block(
            &interleaved_sine(0, 300, 4, 16_000, 440.0),
            16_000,
            4,
            0,
        ))
        .expect("accepted");

    stage
        .accept(&source_block(
            &interleaved_sine(300, 400, 2, 16_000, 440.0),
            16_000,
            2,
            nanoseconds_for(300, 16_000),
        ))
        .expect_err("two channels cannot reach a four-channel contract");

    let flush = stage
        .accept(&source_block(
            &interleaved_sine(700, 100, 4, 16_000, 440.0),
            16_000,
            4,
            nanoseconds_for(700, 16_000),
        ))
        .expect("accepted")
        .expect("the refused block's span left a gap in the run");

    assert_eq!(
        flush.cause,
        AudioWindowStageFlushCause::BlockArrivedAwayFromWhereThePreviousEnded
    );
    assert_eq!(
        flush.discarded_per_channel_samples_at_the_declared_rate,
        300
    );
}

/// A flush on a rolling window counts only the samples no window has carried:
/// the last window's overlap is still in the remainder, and the reader already
/// has it.
///
/// Six hundred samples against a 512/160 contract emit one window and leave 440
/// behind, 352 of them the overlap that window carried. The flush counts the 88
/// no window reached; counting the whole remainder gives 440.
#[test]
fn a_rolling_windows_flush_counts_none_of_the_overlap_its_last_window_delivered() {
    let mut stage = stage_on(contract(16_000, 1, "f32", 512, 160));

    stage
        .accept(&source_block(
            &interleaved_sine(0, 600, 1, 16_000, 440.0),
            16_000,
            1,
            0,
        ))
        .expect("accepted");
    assert_eq!(drain_every_ready_window(&mut stage).len(), 1);

    let flush = stage
        .accept(&source_block(
            &interleaved_sine(0, 100, 1, 16_000, 440.0),
            16_000,
            1,
            NANOSECONDS_PER_SECOND,
        ))
        .expect("accepted")
        .expect("a block a second late flushes");

    assert_eq!(flush.discarded_per_channel_samples_at_the_declared_rate, 88);
}

/// A source that changes its channel count mid-run is a format change like any
/// other: the accumulator flushes rather than emitting one window whose front
/// is stereo and whose back is mono.
#[test]
fn a_source_that_changes_its_channel_count_flushes_rather_than_mixing_two_counts() {
    let mut stage = stage_on(contract_following_the_sources_channels(
        48_000, "f32", 960, 960,
    ));

    // Two thirds of a window in stereo, then the source drops to mono at the
    // sample where the stereo run would have continued.
    stage
        .accept(&source_block(
            &interleaved_sine(0, 640, 2, 48_000, 440.0),
            48_000,
            2,
            0,
        ))
        .expect("accepted");
    assert!(
        stage.next_ready_window().expect("asked").is_none(),
        "640 of 960 frames is not a window"
    );

    for block_index in 0..3u64 {
        let first_frame = 640 + block_index * 480;
        stage
            .accept(&source_block(
                &interleaved_sine(first_frame, 480, 1, 48_000, 440.0),
                48_000,
                1,
                nanoseconds_for(first_frame, 48_000),
            ))
            .expect("accepted");
    }

    let windows = drain_every_ready_window(&mut stage);
    assert_eq!(
        windows.len(),
        1,
        "the stereo remainder was discarded, and the mono run made one window of \
         its own"
    );
    assert_eq!(windows[0].channels, 1);
    assert_eq!(
        windows[0].scalars.len(),
        960,
        "no window carries a scalar from before the count changed"
    );
    assert_eq!(
        windows[0].first_sample_timestamp_ns,
        nanoseconds_for(640, 48_000),
        "the mono run is anchored at its own first block, not the stereo one's"
    );
}
