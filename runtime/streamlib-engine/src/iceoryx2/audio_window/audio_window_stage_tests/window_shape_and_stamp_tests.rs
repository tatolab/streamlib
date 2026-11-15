// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Exact windows at the declared size and hop, stamped from the anchor and the frame index, and the start sample each one carries.

use super::*;

/// The change file's flagship case, asserted on the signal rather than on
/// block counts: 48 kHz stereo in, exactly-512-sample mono windows at 16 kHz
/// out, stamps advancing by exactly 32 ms within a contiguous run.
#[test]
fn a_48k_stereo_source_reaches_a_16k_mono_512_port_as_exact_windows_32ms_apart() {
    let contract = contract(16_000, 1, "f32", 512, 512);
    let mut stage = stage_on(contract);

    let source_frames_per_block = 512;
    let mut windows = Vec::new();
    for block_index in 0..40u64 {
        let first_frame = block_index * source_frames_per_block;
        let block = source_block(
            &interleaved_sine(
                first_frame,
                source_frames_per_block as usize,
                2,
                48_000,
                440.0,
            ),
            48_000,
            2,
            nanoseconds_for(first_frame, 48_000),
        );
        stage.accept(&block).expect("the stage accepts the block");
        windows.extend(drain_every_ready_window(&mut stage));
    }

    assert!(
        windows.len() >= 10,
        "40 blocks of 512 frames at 48 kHz is ~6800 frames at 16 kHz, so at least ten \
         512-sample windows; got {}",
        windows.len()
    );
    for window in &windows {
        assert_eq!(window.sample_count, 512);
        assert_eq!(window.channels, 1);
        assert_eq!(window.sample_rate, 16_000);
        assert_eq!(
            window.scalars.len(),
            512,
            "a window carries window_size × channels scalars"
        );
    }

    for pair in windows.windows(2) {
        assert_eq!(
            pair[1].first_sample_timestamp_ns - pair[0].first_sample_timestamp_ns,
            32_000_000,
            "512 samples at 16 kHz is exactly 32 ms, and the stamps say so"
        );
    }
}

/// The stage derives every stamp from the anchor and the frame index, so the
/// first window is stamped at the device's own stamp — the group delay is paid
/// by discarding the priming, not by dating the audio late.
#[test]
fn the_first_window_carries_the_anchor_stamp_rather_than_one_a_group_delay_later() {
    let contract = contract(16_000, 1, "f32", 512, 512);
    let mut stage = stage_on(contract);

    let anchor_ns = 4_000_000_000;
    let mut windows = Vec::new();
    for block_index in 0..12u64 {
        let first_frame = block_index * 512;
        let block = source_block(
            &interleaved_sine(first_frame, 512, 2, 48_000, 440.0),
            48_000,
            2,
            anchor_ns + nanoseconds_for(first_frame, 48_000),
        );
        stage.accept(&block).expect("accepted");
        windows.extend(drain_every_ready_window(&mut stage));
    }

    assert_eq!(
        windows
            .first()
            .expect("a window emitted")
            .first_sample_timestamp_ns,
        anchor_ns
    );
}

/// A rolling window, proven by comparing sample values across consecutive
/// windows rather than by counting them. The rates already agree, so the
/// scalars pass through untouched and the overlap is exact.
#[test]
fn a_hop_of_160_against_a_window_of_512_overlaps_by_352_samples() {
    let contract = contract(16_000, 1, "f32", 512, 160);
    let mut stage = stage_on(contract);

    for block_index in 0..8u64 {
        let first_frame = block_index * 512;
        let block = source_block(
            &interleaved_sine(first_frame, 512, 1, 16_000, 300.0),
            16_000,
            1,
            nanoseconds_for(first_frame, 16_000),
        );
        stage.accept(&block).expect("accepted");
    }
    let windows = drain_every_ready_window(&mut stage);

    assert!(windows.len() >= 3, "got {} windows", windows.len());
    for pair in windows.windows(2) {
        assert_eq!(
            &pair[0].scalars[160..512],
            &pair[1].scalars[0..352],
            "consecutive windows share the 352 samples the hop did not advance past"
        );
        assert_eq!(
            pair[1].first_sample_timestamp_ns - pair[0].first_sample_timestamp_ns,
            10_000_000,
            "160 samples at 16 kHz is exactly 10 ms"
        );
    }
}

/// One 1024-sample quantum against a 512/512 contract satisfies exactly two
/// windows — the count the drain loop dispatches `process()`.
#[test]
fn one_1024_sample_quantum_against_a_512_512_contract_yields_exactly_two_windows() {
    let contract = contract(16_000, 1, "f32", 512, 512);
    let mut stage = stage_on(contract);

    stage
        .accept(&source_block(
            &interleaved_sine(0, 1024, 1, 16_000, 300.0),
            16_000,
            1,
            0,
        ))
        .expect("accepted");

    let windows = drain_every_ready_window(&mut stage);
    assert_eq!(windows.len(), 2);
    assert!(
        stage.next_ready_window().expect("asked again").is_none(),
        "a third window would have to invent samples the quantum did not carry"
    );
}

/// A stream that simply stops leaves under one window parked, delivered to
/// nothing — designed, not a defect: an exact-size contract has no partial
/// form to hand over.
#[test]
fn a_stream_that_stops_mid_window_hands_over_nothing_rather_than_a_short_block() {
    let contract = contract(16_000, 1, "f32", 512, 512);
    let mut stage = stage_on(contract);

    stage
        .accept(&source_block(
            &interleaved_sine(0, 300, 1, 16_000, 300.0),
            16_000,
            1,
            0,
        ))
        .expect("accepted");

    assert!(stage.next_ready_window().expect("asked").is_none());
}

/// Each window's start sample counts the declared rate from the first window,
/// so consecutive windows step by the hop and a gap advances it by what it
/// lost.
#[test]
fn a_windows_start_sample_steps_by_the_hop_and_a_gap_advances_it() {
    let contract = contract(16_000, 1, "f32", 256, 128);
    let mut stage = stage_on(contract);
    stage
        .accept(&source_block(&[0.0; 512], 16_000, 1, 0))
        .expect("accepted");
    let start_samples_of = |stage: &mut AudioWindowAccumulator| -> Vec<u64> {
        std::iter::from_fn(|| stage.next_ready_window().expect("asked"))
            .map(|window| {
                channels_block_body_of(&window.message.payload_descriptor)
                    .expect("a window is a Channels block")
                    .start_sample
            })
            .collect()
    };
    assert_eq!(start_samples_of(&mut stage), [0, 128, 256]);

    stage
        .accept(&source_block(
            &[0.0; 256],
            16_000,
            1,
            NANOSECONDS_PER_SECOND,
        ))
        .expect("accepted after a gap");
    assert_eq!(
        start_samples_of(&mut stage),
        [16_000],
        "the run after a one-second gap starts a second's samples on"
    );
}
