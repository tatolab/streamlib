// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Channels averaged, duplicated or passed through as the contract declares,
//! the sources it refuses, and the sample type a window carries.

use super::*;

#[test]
fn a_stereo_source_reaching_a_mono_contract_is_averaged_across_its_channels() {
    let contract = contract(16_000, 1, "f32", 512, 512);
    let mut stage = stage_on(contract);

    // Left at +0.5, right at -0.25 — an average of +0.125 the test can name.
    let interleaved: Vec<f32> = (0..512).flat_map(|_| [0.5f32, -0.25f32]).collect();
    stage
        .accept(&source_block(&interleaved, 16_000, 2, 0))
        .expect("accepted");

    let windows = drain_every_ready_window(&mut stage);
    assert_eq!(windows.len(), 1);
    for sample in &windows[0].scalars {
        assert!(
            (sample - 0.125).abs() < 1e-6,
            "a mono contract averages its source's channels; got {sample}"
        );
    }
}

#[test]
fn a_mono_source_reaching_a_stereo_contract_is_duplicated_across_its_channels() {
    let contract = contract(16_000, 2, "f32", 256, 256);
    let mut stage = stage_on(contract);

    let interleaved: Vec<f32> = (0..256).map(|index| index as f32 / 256.0).collect();
    stage
        .accept(&source_block(&interleaved, 16_000, 1, 0))
        .expect("accepted");

    let windows = drain_every_ready_window(&mut stage);
    assert_eq!(windows.len(), 1);
    assert_eq!(windows[0].scalars.len(), 512, "256 frames × 2 channels");
    for (frame, pair) in windows[0].scalars.as_chunks::<2>().0.iter().enumerate() {
        assert_eq!(
            pair[0], pair[1],
            "both channels carry the same source sample"
        );
        assert!((pair[0] - frame as f32 / 256.0).abs() < 1e-6);
    }
}

#[test]
fn a_channel_pair_with_neither_side_at_one_is_refused_naming_both_counts() {
    let contract = contract(16_000, 4, "f32", 256, 256);
    let mut stage = stage_on(contract);

    let refusal = stage
        .accept(&source_block(&vec![0.0f32; 512], 16_000, 2, 0))
        .expect_err("2 to 4 is neither a mixdown nor a duplication");
    let rendered = refusal.to_string();
    assert!(
        rendered.contains('2') && rendered.contains('4') && rendered.contains("audio"),
        "the refusal must name both counts and the port; got {rendered}"
    );
}

#[test]
fn a_bag_the_stage_cannot_read_is_refused_by_name_rather_than_reshaped() {
    let contract = contract(16_000, 1, "f32", 256, 256);
    let mut stage = stage_on(contract);

    let not_a_channels_block = MessageReadFromInputPort {
        payload_descriptor: DataChannelPayloadDescriptor::bytes(),
        ..source_block(&[0.0; 256], 16_000, 1, 0)
    };
    let refusal = stage
        .accept(&not_a_channels_block)
        .expect_err("a message of another kind is refused");
    let rendered = refusal.to_string();
    assert!(
        rendered.contains("audio") && rendered.contains("Channels blocks only"),
        "the refusal must name the port and what it could not read; got {rendered}"
    );
}

/// An `f64` contract is the same stage with a different sample type, so the
/// windows stay exact and their samples come back as the block says.
#[test]
fn an_f64_contract_emits_windows_whose_samples_are_f64() {
    let contract = contract(16_000, 1, "f64", 256, 256);
    let mut stage = stage_on(contract);

    let interleaved: Vec<f32> = (0..256).map(|_| 0.5f32).collect();
    stage
        .accept(&source_block(&interleaved, 16_000, 1, 0))
        .expect("accepted");

    let window = stage
        .next_ready_window()
        .expect("a window emits")
        .expect("a full window");
    let body = channels_block_body_of(&window.message.payload_descriptor)
        .expect("a window is a Channels block");
    assert_eq!(body.sample_type, ChannelsBlockSampleType::F64 as u32);
    assert_eq!(window.message.payload.len(), 256 * 8);
    for scalar in window.message.payload.as_chunks::<8>().0.iter() {
        assert_eq!(f64::from_le_bytes(*scalar), 0.5);
    }
}

/// The ticket's flagship case: a contract stating everything but its count
/// carries the source's own through untouched, so a graph can grow a stereo
/// microphone without every consumer downstream of it being edited.
#[test]
fn a_contract_declaring_no_channels_emits_the_sources_own_count() {
    let mut stage = stage_on(contract_following_the_sources_channels(
        48_000, "f32", 960, 960,
    ));

    let mut windows = Vec::new();
    for block_index in 0..8u64 {
        let first_frame = block_index * 960;
        stage
            .accept(&source_block(
                &interleaved_sine(first_frame, 960, 2, 48_000, 440.0),
                48_000,
                2,
                nanoseconds_for(first_frame, 48_000),
            ))
            .expect("the stage accepts a stereo block");
        windows.extend(drain_every_ready_window(&mut stage));
    }

    assert_eq!(
        windows.len(),
        8,
        "one 960-frame block is exactly one window"
    );
    for window in &windows {
        assert_eq!(window.channels, 2, "the source's count, not a declared one");
        assert_eq!(window.sample_count, 960);
        assert_eq!(window.sample_rate, 48_000);
        assert_eq!(
            window.scalars.len(),
            1_920,
            "a window carries window_size × the source's channels"
        );
    }
    for pair in windows.windows(2) {
        assert_eq!(
            pair[1].first_sample_timestamp_ns - pair[0].first_sample_timestamp_ns,
            20_000_000,
            "960 samples at 48 kHz is exactly 20 ms"
        );
    }
}

/// The same contract against a mono source: following means following, not
/// defaulting to a count the contract secretly holds.
#[test]
fn the_same_channel_free_contract_emits_mono_from_a_mono_source() {
    let mut stage = stage_on(contract_following_the_sources_channels(
        48_000, "f32", 960, 960,
    ));

    stage
        .accept(&source_block(
            &interleaved_sine(0, 960, 1, 48_000, 440.0),
            48_000,
            1,
            0,
        ))
        .expect("the stage accepts a mono block");

    let windows = drain_every_ready_window(&mut stage);
    assert_eq!(windows.len(), 1);
    assert_eq!(windows[0].channels, 1);
    assert_eq!(windows[0].scalars.len(), 960);
}

/// Following the source still resamples: only the channel-convert step is
/// skipped, and the rest of the fixed order is exactly as it was.
#[test]
fn a_channel_free_contract_still_resamples_to_the_rate_it_declared() {
    let mut stage = stage_on(contract_following_the_sources_channels(
        16_000, "f32", 512, 512,
    ));

    for block_index in 0..40u64 {
        let first_frame = block_index * 512;
        stage
            .accept(&source_block(
                &interleaved_sine(first_frame, 512, 2, 48_000, 440.0),
                48_000,
                2,
                nanoseconds_for(first_frame, 48_000),
            ))
            .expect("accepted");
    }

    let windows = drain_every_ready_window(&mut stage);
    assert!(
        !windows.is_empty(),
        "40 blocks make several 512-sample windows"
    );
    for window in &windows {
        assert_eq!(window.sample_rate, 16_000, "the declared rate is honoured");
        assert_eq!(window.channels, 2, "the source's count rode through it");
        assert_eq!(window.scalars.len(), 1_024);
    }
}

/// A count nobody declared cannot be an N→M pair, so the refusal that guards a
/// declared one must not fire here — six channels reach a channel-free port
/// untouched where a declared stereo contract would have refused them.
#[test]
fn a_source_a_declared_pair_would_refuse_rides_a_channel_free_contract_through() {
    let mut stage = stage_on(contract_following_the_sources_channels(
        48_000, "f32", 960, 960,
    ));

    stage
        .accept(&source_block(
            &interleaved_sine(0, 960, 6, 48_000, 440.0),
            48_000,
            6,
            0,
        ))
        .expect("six channels into a contract that declared none is not a conversion");

    let windows = drain_every_ready_window(&mut stage);
    assert_eq!(windows.len(), 1);
    assert_eq!(windows[0].channels, 6);
    assert_eq!(windows[0].scalars.len(), 5_760);

    let declared_stereo = contract(48_000, 2, "f32", 960, 960);
    let mut refusing = stage_on(declared_stereo);
    refusing
        .accept(&source_block(
            &interleaved_sine(0, 960, 6, 48_000, 440.0),
            48_000,
            6,
            0,
        ))
        .expect_err("a declared 6→2 pair with neither side at one is still refused");
}
