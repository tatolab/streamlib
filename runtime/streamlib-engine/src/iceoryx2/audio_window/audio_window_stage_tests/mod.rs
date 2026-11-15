// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! What the stage promises, asserted on known signals rather than on block
//! counts: exact windows, exact stamps, overlap proven sample by sample, and a
//! gap that flushes the filter instead of blending audio across it.

use std::sync::Arc;

use streamlib_processor_schema::AudioWindowContractDeclaredValues;

use super::super::input::MessageReadFromInputPort;
use super::super::{ChannelsBlockSampleType, DataChannelPayloadDescriptor, MessageGroupBoundary};
use super::audio_window_accumulator::{
    AudioWindowAccumulator, AudioWindowStageFlushCause, LatestQueuedSourceAudioFormat,
    SourceAudioFormat,
};
use super::channels_block_of_the_window_stage::{
    ChannelsBlockReadByTheWindowStage, channels_block_body_of, channels_block_of_one_window,
    read_a_channels_block_for_the_window_stage,
};
use super::resolved_audio_window_contract::ResolvedAudioWindowContract;

mod channel_and_sample_format_tests;
mod gap_and_flush_tests;
mod readiness_floor_tests;
mod window_shape_and_stamp_tests;

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

/// One source block as the stage receives it: `frames` per-channel samples
/// interleaved by `channels` written as a planar f32 Channels block, stamped at
/// `first_sample_timestamp_ns`.
fn source_block(
    interleaved: &[f32],
    sample_rate: u32,
    channels: u32,
    first_sample_timestamp_ns: i64,
) -> MessageReadFromInputPort {
    let (payload_descriptor, planar_payload) = channels_block_of_one_window(
        interleaved,
        sample_rate,
        channels,
        interleaved.len() as u32 / channels,
        ChannelsBlockSampleType::F32,
        0,
    );
    MessageReadFromInputPort {
        payload: planar_payload.into(),
        timestamp_ns: first_sample_timestamp_ns,
        group_boundary: MessageGroupBoundary::StartsGroup,
        lost_since_last_read: 0,
        payload_descriptor,
    }
}

/// An emitted window read back as the Channels block it is.
fn channels_block_of_the_window(
    window: &MessageReadFromInputPort,
) -> ChannelsBlockReadByTheWindowStage<'_> {
    read_a_channels_block_for_the_window_stage(
        &window.payload_descriptor,
        &window.payload,
        window.timestamp_ns,
    )
    .expect("an emitted window reads back")
}

/// One emitted window, read back as the Channels block it is.
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
        let block = channels_block_of_the_window(&window.message);
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
