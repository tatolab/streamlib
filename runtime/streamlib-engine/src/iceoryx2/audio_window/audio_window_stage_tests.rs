// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! What the stage promises, asserted on known signals rather than on block
//! counts: exact windows, exact stamps, overlap proven sample by sample, and a
//! gap that flushes the filter instead of blending audio across it.

use std::sync::Arc;

use streamlib_processor_schema::AudioWindowContractDeclaredValues;

use super::audio_block_bag_wire_codec::{
    AudioBlockSampleDtype, encode_an_audio_block_onto_the_wire, read_an_audio_block_off_the_wire,
};
use super::audio_window_accumulator::{
    AudioWindowAccumulator, LatestQueuedSourceAudioFormat, SourceAudioFormat,
};
use super::resolved_audio_window_contract::ResolvedAudioWindowContract;

const NANOSECONDS_PER_SECOND: i64 = 1_000_000_000;

fn contract(
    sample_rate: u32,
    channels: u32,
    dtype: &str,
    window_size: u32,
    hop: u32,
) -> ResolvedAudioWindowContract {
    a_contract_stating(sample_rate, Some(channels), dtype, window_size, hop)
}

/// A contract that states everything but its channel count, so every emitted
/// window carries whatever count the source sent.
fn contract_following_the_sources_channels(
    sample_rate: u32,
    dtype: &str,
    window_size: u32,
    hop: u32,
) -> ResolvedAudioWindowContract {
    a_contract_stating(sample_rate, None, dtype, window_size, hop)
}

fn a_contract_stating(
    sample_rate: u32,
    channels: Option<u32>,
    dtype: &str,
    window_size: u32,
    hop: u32,
) -> ResolvedAudioWindowContract {
    ResolvedAudioWindowContract::from_declared_values(&AudioWindowContractDeclaredValues {
        sample_rate,
        channels,
        dtype: dtype.to_string(),
        window_size,
        hop,
    })
    .expect("a contract the stage can honour")
}

/// A stage on a port nothing has queued a bag into yet — every test drives it
/// through `accept`, which is what a read does once the gate has cleared.
fn stage_on(contract: ResolvedAudioWindowContract) -> AudioWindowAccumulator {
    AudioWindowAccumulator::new(
        "audio",
        contract,
        Arc::new(LatestQueuedSourceAudioFormat::default()),
    )
}

/// A stage plus the source-format cell its port's mailbox measure writes into.
/// The readiness tests drive that cell themselves, because it is what makes the
/// floor exact before a single bag has been consumed.
fn stage_and_the_format_its_mailbox_reports(
    contract: ResolvedAudioWindowContract,
) -> (AudioWindowAccumulator, Arc<LatestQueuedSourceAudioFormat>) {
    let latest_queued_source_audio_format = Arc::new(LatestQueuedSourceAudioFormat::default());
    (
        AudioWindowAccumulator::new(
            "audio",
            contract,
            Arc::clone(&latest_queued_source_audio_format),
        ),
        latest_queued_source_audio_format,
    )
}

/// One source block on the wire, as the stage receives it: `frames` per-channel
/// samples interleaved by `channels`, stamped at `first_sample_timestamp_ns`.
fn source_block(
    interleaved: &[f32],
    sample_rate: u32,
    channels: u32,
    first_sample_timestamp_ns: i64,
) -> Vec<u8> {
    encode_an_audio_block_onto_the_wire(
        interleaved,
        sample_rate,
        channels,
        interleaved.len() as u32 / channels,
        AudioBlockSampleDtype::F32,
        first_sample_timestamp_ns,
    )
    .expect("a source block encodes")
}

/// One emitted window, read back as the audio block it is.
struct EmittedWindow {
    scalars: Vec<f32>,
    sample_rate: u32,
    channels: u32,
    sample_count: u32,
    first_sample_timestamp_ns: i64,
}

fn drain_every_ready_window(stage: &mut AudioWindowAccumulator) -> Vec<EmittedWindow> {
    let mut windows = Vec::new();
    while let Some(window) = stage.next_ready_window().expect("a window emits") {
        let block =
            read_an_audio_block_off_the_wire(&window.body).expect("an emitted window reads back");
        assert_eq!(
            block.first_sample_timestamp_ns, window.first_sample_or_publish_timestamp_ns,
            "the stamp beside the bag is the one inside it"
        );
        windows.push(EmittedWindow {
            scalars: block.interleaved_samples_as_f32().collect(),
            sample_rate: block.sample_rate,
            channels: block.channels,
            sample_count: block.sample_count,
            first_sample_timestamp_ns: block.first_sample_timestamp_ns,
        });
    }
    windows
}

/// A full-scale sine, sampled from `first_frame` for `frames` frames and
/// duplicated across `channels`.
fn interleaved_sine(
    first_frame: u64,
    frames: usize,
    channels: u32,
    sample_rate: u32,
    hertz: f64,
) -> Vec<f32> {
    (0..frames)
        .flat_map(|offset| {
            let instant = (first_frame + offset as u64) as f64 / f64::from(sample_rate);
            let sample = (std::f64::consts::TAU * hertz * instant).sin() as f32;
            std::iter::repeat_n(sample, channels as usize)
        })
        .collect()
}

fn nanoseconds_for(frames: u64, sample_rate: u32) -> i64 {
    (frames * NANOSECONDS_PER_SECOND as u64 / u64::from(sample_rate)) as i64
}

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
    for (frame, pair) in windows[0].scalars.chunks_exact(2).enumerate() {
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

    let not_an_audio_block =
        rmp_serde::to_vec_named(&serde_json::json!({ "width": 1920 })).expect("encodes");
    let refusal = stage
        .accept(&not_an_audio_block)
        .expect_err("a bag with no audio-block keys is refused");
    let rendered = refusal.to_string();
    assert!(
        rendered.contains("audio") && rendered.contains("audio block"),
        "the refusal must name the port and what it could not read; got {rendered}"
    );
}

/// An `i16` contract is the same stage with a different encode, so the windows
/// stay exact and the scalars come back as the wire says they were written.
#[test]
fn an_i16_contract_emits_windows_whose_scalars_are_written_as_i16() {
    let contract = contract(16_000, 1, "i16", 256, 256);
    let mut stage = stage_on(contract);

    let interleaved: Vec<f32> = (0..256).map(|_| 0.5f32).collect();
    stage
        .accept(&source_block(&interleaved, 16_000, 1, 0))
        .expect("accepted");

    let window = stage
        .next_ready_window()
        .expect("a window emits")
        .expect("a full window");
    let block = read_an_audio_block_off_the_wire(&window.body).expect("reads back");
    assert_eq!(block.dtype, AudioBlockSampleDtype::I16);
    assert_eq!(block.interleaved_sample_bytes.len(), 256 * 2);
    for scalar in block.interleaved_sample_bytes.chunks_exact(2) {
        assert_eq!(i16::from_le_bytes([scalar[0], scalar[1]]), 16_384);
    }
}

/// Drive the readiness floor over one source format, asserting the two things
/// that together make it meaningful: every window it promises can actually be
/// produced, in the count the contract resolves to — and it clears at all,
/// since a floor that never says yes satisfies the first vacuously and would
/// leave a reactive processor undispatched forever.
///
/// Small quanta on purpose: the queue must cross one window's worth in steps
/// smaller than the priming and chunk slack, or a floor blind to that slack
/// steps straight over the gap where it would overclaim.
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
        let emitted = read_an_audio_block_off_the_wire(&window.body).expect("reads back");
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
    let mut queued: Vec<Vec<u8>> = (0..3u64)
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
    let block = read_an_audio_block_off_the_wire(&window.body).expect("reads back");
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

/// And it must eventually say yes, or a reactive processor on a windowed port
/// would never be dispatched at all.
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

// ---- a contract that declares no channel count ----------------------------

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
