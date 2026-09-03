// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The audio window contract's read-side stage — resample, mix down and frame
//! natively, so `process()` receives exact-size timestamped blocks.
//!
//! One stage at the one read seam every reader already shares
//! ([`InputMailboxesInner::read_raw_bounded`]): an app-process Rust processor
//! reads through the parent's mailboxes, and a helper-placed Python processor
//! through its own, which the child opens for itself and which compiles this
//! same code into the wheel. One implementation serving both, with no new IPC
//! hop and no parent↔child contract to design.
//!
//! The stage sits in the port's mailbox rather than in any runner, so it is
//! execution-mode agnostic: a window is exact whoever reads it, and `has_data`
//! reports a full window on every mode. What differs is only dispatch. A
//! reactive processor is gated on readiness — being woken with nothing to read
//! is the shape this contract exists to rule out. A `continuous` one ticks on
//! its own timer and a `manual` one drives itself, so both may call `read` when
//! no window is ready yet and get `None`; that is those modes' own semantics,
//! not something the contract changes, and the blocks they do receive are the
//! declared size like everyone else's.
//!
//! [`InputMailboxesInner::read_raw_bounded`]: crate::iceoryx2::InputMailboxesInner::read_raw_bounded

mod audio_block_bag_wire_codec;
mod audio_window_accumulator;
mod device_matched_audio_window_contracts;
mod resolved_audio_window_contract;

#[cfg(test)]
mod audio_window_stage_tests;

use std::sync::Arc;

pub(crate) use audio_window_accumulator::{
    AudioWindowAccumulator, LatestQueuedSourceAudioFormat, SourceAudioFormat,
};
pub use device_matched_audio_window_contracts::{
    AudioWindowContractMatchingADeviceStream, DeviceMatchedAudioWindowContractsByInputPort,
};
pub use resolved_audio_window_contract::ResolvedAudioWindowContract;
pub(crate) use resolved_audio_window_contract::{
    AudioWindowDeclarationOfAnInputPort, audio_windowing_declared_by_input_port,
    refuse_an_unsettled_match_device_sentinel,
};

use audio_block_bag_wire_codec::read_an_audio_block_off_the_wire;

use super::FrameHeader;
use super::mailbox::PortMailboxQueuedFrameMeasure;

/// A measure over one queued wire frame: what the audio block it carries is
/// worth in frames at `contract`'s rate, and the format it stated.
///
/// Installed on a windowed port's mailbox so the readiness gate can ask how
/// much is queued without consuming any of it. `crossbeam`'s `ArrayQueue`
/// admits no peek — pushing and popping is the whole of its API — so the
/// measure is taken once, as the frame arrives, and carried with it. A frame
/// this cannot read measures zero: the read path is where a bag is refused by
/// name, and a readiness gate that raised there would refuse it before any
/// reader could be told which port it arrived on.
///
/// `latest_source_audio_format` is the same reading in the terms the stage
/// needs to build its rate conversion, so the very first readiness question —
/// asked before any bag has been consumed — is answered against the real
/// filter delay rather than a worst case. The channel count is part of it
/// because a contract that declared none resamples across the source's own.
pub(crate) fn queued_audio_window_frame_measure(
    contract: ResolvedAudioWindowContract,
    latest_source_audio_format: Arc<LatestQueuedSourceAudioFormat>,
) -> PortMailboxQueuedFrameMeasure {
    Arc::new(move |wire_frame: &[u8]| -> u64 {
        // `read_payload_from_slice` is the one place the frame's stamped
        // length is trusted against what the frame actually carries; a
        // truncated frame's leading bytes are a well-formed shorter message in
        // every self-describing wire format, which is the trap it exists for.
        let Some(body) = FrameHeader::read_payload_from_slice(wire_frame) else {
            return 0;
        };

        let Ok(block) = read_an_audio_block_off_the_wire(body) else {
            return 0;
        };
        if block.sample_rate == 0 || block.channels == 0 {
            return 0;
        }
        latest_source_audio_format.record(SourceAudioFormat {
            sample_rate: block.sample_rate,
            channels: block.channels,
        });
        // Per-channel frames on both sides, so the measure needs no count.
        u64::from(block.sample_count) * u64::from(contract.sample_rate)
            / u64::from(block.sample_rate)
    })
}
