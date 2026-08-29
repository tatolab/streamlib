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
//! [`InputMailboxesInner::read_raw_bounded`]: crate::iceoryx2::InputMailboxesInner::read_raw_bounded

mod audio_block_bag_wire_codec;
mod audio_window_accumulator;
mod resolved_audio_window_contract;

#[cfg(test)]
mod audio_window_stage_tests;

use std::sync::Arc;

pub(crate) use audio_window_accumulator::AudioWindowAccumulator;
pub use resolved_audio_window_contract::ResolvedAudioWindowContract;
pub(crate) use resolved_audio_window_contract::audio_window_contract_for_input_port;

use audio_block_bag_wire_codec::read_an_audio_block_off_the_wire;

use super::mailbox::QueuedFrameMeasure;
use super::{FRAME_HEADER_SIZE, FrameHeader};

/// A measure over one queued wire frame: what the audio block it carries is
/// worth in frames at `contract`'s rate.
///
/// Installed on a windowed port's mailbox so the readiness gate can ask how
/// much is queued without consuming any of it. `crossbeam`'s `ArrayQueue`
/// admits no peek — pushing and popping is the whole of its API — so the
/// measure is taken once, as the frame arrives, and carried with it. A frame
/// this cannot read measures zero: the read path is where a bag is refused by
/// name, and a readiness gate that raised there would refuse it before any
/// reader could be told which port it arrived on.
pub(crate) fn queued_audio_window_frame_measure(
    contract: ResolvedAudioWindowContract,
) -> QueuedFrameMeasure {
    Arc::new(move |wire_frame: &[u8]| -> u64 {
        if wire_frame.len() < FRAME_HEADER_SIZE {
            return 0;
        }
        let header = FrameHeader::read_from_slice(wire_frame);
        let stamped_payload_bytes = header.len as usize;
        let available_payload_bytes = wire_frame.len() - FRAME_HEADER_SIZE;
        if stamped_payload_bytes > available_payload_bytes {
            return 0;
        }
        let body = &wire_frame[FRAME_HEADER_SIZE..FRAME_HEADER_SIZE + stamped_payload_bytes];

        let Ok(block) = read_an_audio_block_off_the_wire(body) else {
            return 0;
        };
        if block.sample_rate == 0 {
            return 0;
        }
        u64::from(block.sample_count) * u64::from(contract.sample_rate)
            / u64::from(block.sample_rate)
    })
}
