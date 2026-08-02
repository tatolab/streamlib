// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The 24-byte measurement preamble the spike prepends to every frame payload.
//!
//! Wire contract (little-endian, packed, no padding). The source writes the
//! first two fields, the stage fills in the third, the sink reads all three:
//!
//! ```text
//! offset 0..8   u64  frame sequence number, starting at 0, +1 per emitted frame
//! offset 8..16  i64  source emit stamp, raw CLOCK_MONOTONIC nanoseconds
//! offset 16..24 i64  stage callback duration in nanoseconds, 0 until the stage fills it
//! offset 24..   [u8] the frame pixel payload
//! ```
//!
//! The stage duration rides the frame rather than a side channel so it is
//! correlated to its own frame by construction — a cross-thread channel would
//! need the sequence number to re-associate them and could interleave under
//! load, silently attributing one frame's stall to another.
//!
//! Both fields have to ride the payload rather than the engine's own metadata:
//! `FrameHeader` carries no sequence field
//! (`runtime/streamlib-ipc-types/src/lib.rs:210` — the 204-byte header is port
//! key + schema ident + timestamp + length), and `@tatolab/core/VideoFrame`
//! deliberately removed its `frame_index` field, with the schema stating
//! `timestamp_ns` is the sole ordering primitive
//! (`packages/core/schemas/video_frame.yaml:43`). Re-adding either would be an
//! engine change the spike forbids.
//!
//! A raw preamble rather than a msgpack named map: the ports are declared `any`,
//! so no schema is ever consulted, and msgpack would cost a full encode+decode
//! of an 8.3 MB body on every hop — contaminating the exact number being
//! measured.

use crate::monotonic_clock::MonotonicNanoseconds;

/// Byte length of the preamble prefixed to every spike frame payload.
pub const SYNTHETIC_FRAME_MEASUREMENT_PREAMBLE_BYTES: usize = 24;

/// Byte offset of the stage-duration field, which the stage patches in place
/// without rewriting the fields the source owns.
const STAGE_CALLBACK_NANOSECONDS_OFFSET: usize = 16;

/// The per-frame measurement fields carried in-band ahead of the pixel payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SyntheticFrameMeasurementPreamble {
    /// Emission ordinal, starting at 0. Gaps observed at the sink are dropped
    /// frames; the sink counts them rather than inferring drops from timing.
    pub frame_sequence_number: u64,
    /// Raw `CLOCK_MONOTONIC` nanoseconds captured immediately before the source
    /// handed the frame to its output port.
    pub source_emit_monotonic_nanoseconds: MonotonicNanoseconds,
    /// Wall time spent inside the stage's callback. Zero on a frame that has not
    /// yet passed a stage, and zero for the floor arm's no-op stage.
    pub stage_callback_nanoseconds: i64,
}

impl SyntheticFrameMeasurementPreamble {
    /// Serialize into the first [`SYNTHETIC_FRAME_MEASUREMENT_PREAMBLE_BYTES`]
    /// of `payload_buffer`.
    ///
    /// Returns `false` if the buffer is too short to hold the preamble.
    pub fn write_into_payload_prefix(&self, payload_buffer: &mut [u8]) -> bool {
        if payload_buffer.len() < SYNTHETIC_FRAME_MEASUREMENT_PREAMBLE_BYTES {
            return false;
        }
        payload_buffer[0..8].copy_from_slice(&self.frame_sequence_number.to_le_bytes());
        payload_buffer[8..16]
            .copy_from_slice(&self.source_emit_monotonic_nanoseconds.to_le_bytes());
        payload_buffer[16..24].copy_from_slice(&self.stage_callback_nanoseconds.to_le_bytes());
        true
    }

    /// Patch only the stage-duration field, leaving the source-owned fields
    /// byte-identical.
    ///
    /// Returns `false` if the buffer is too short to hold the preamble.
    pub fn patch_stage_callback_nanoseconds_in_payload_prefix(
        payload_buffer: &mut [u8],
        stage_callback_nanoseconds: i64,
    ) -> bool {
        if payload_buffer.len() < SYNTHETIC_FRAME_MEASUREMENT_PREAMBLE_BYTES {
            return false;
        }
        payload_buffer
            [STAGE_CALLBACK_NANOSECONDS_OFFSET..STAGE_CALLBACK_NANOSECONDS_OFFSET + 8]
            .copy_from_slice(&stage_callback_nanoseconds.to_le_bytes());
        true
    }

    /// Parse from the first [`SYNTHETIC_FRAME_MEASUREMENT_PREAMBLE_BYTES`] of
    /// `payload_buffer`, returning `None` if the buffer is too short.
    pub fn read_from_payload_prefix(payload_buffer: &[u8]) -> Option<Self> {
        if payload_buffer.len() < SYNTHETIC_FRAME_MEASUREMENT_PREAMBLE_BYTES {
            return None;
        }
        let mut sequence_bytes = [0u8; 8];
        sequence_bytes.copy_from_slice(&payload_buffer[0..8]);
        let mut emit_stamp_bytes = [0u8; 8];
        emit_stamp_bytes.copy_from_slice(&payload_buffer[8..16]);
        let mut stage_duration_bytes = [0u8; 8];
        stage_duration_bytes.copy_from_slice(&payload_buffer[16..24]);
        Some(Self {
            frame_sequence_number: u64::from_le_bytes(sequence_bytes),
            source_emit_monotonic_nanoseconds: i64::from_le_bytes(emit_stamp_bytes),
            stage_callback_nanoseconds: i64::from_le_bytes(stage_duration_bytes),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The wire contract must survive a write/read cycle exactly — every
    /// latency number and every drop count is derived from these three fields.
    #[test]
    fn preamble_round_trips_through_the_payload_prefix() {
        let original = SyntheticFrameMeasurementPreamble {
            frame_sequence_number: 0x0123_4567_89ab_cdef,
            source_emit_monotonic_nanoseconds: 1_234_567_890_123_456,
            stage_callback_nanoseconds: 4_200_000,
        };
        let mut payload_buffer = vec![0u8; 64];
        assert!(original.write_into_payload_prefix(&mut payload_buffer));
        let parsed = SyntheticFrameMeasurementPreamble::read_from_payload_prefix(&payload_buffer)
            .expect("a 64-byte buffer holds the preamble");
        assert_eq!(parsed, original);
    }

    /// Writing must not disturb the pixel payload that follows it — the Python
    /// stage receives a view starting at the preamble's end and must see the
    /// bytes the source wrote.
    #[test]
    fn preamble_write_leaves_the_pixel_payload_untouched() {
        let mut payload_buffer = vec![0xAAu8; 128];
        let preamble = SyntheticFrameMeasurementPreamble {
            frame_sequence_number: 7,
            source_emit_monotonic_nanoseconds: 99,
            stage_callback_nanoseconds: 0,
        };
        assert!(preamble.write_into_payload_prefix(&mut payload_buffer));
        assert!(
            payload_buffer[SYNTHETIC_FRAME_MEASUREMENT_PREAMBLE_BYTES..]
                .iter()
                .all(|&byte| byte == 0xAA),
            "the preamble write overran into the pixel payload"
        );
    }

    /// The stage patches its duration in place; if that patch disturbed the
    /// source's sequence number or emit stamp, every latency number downstream
    /// would be silently wrong rather than obviously broken.
    #[test]
    fn patching_the_stage_duration_leaves_the_source_fields_intact() {
        let source_written = SyntheticFrameMeasurementPreamble {
            frame_sequence_number: 4242,
            source_emit_monotonic_nanoseconds: 987_654_321,
            stage_callback_nanoseconds: 0,
        };
        let mut payload_buffer = vec![0xCDu8; 96];
        assert!(source_written.write_into_payload_prefix(&mut payload_buffer));
        assert!(
            SyntheticFrameMeasurementPreamble::patch_stage_callback_nanoseconds_in_payload_prefix(
                &mut payload_buffer,
                3_500_000,
            )
        );

        let parsed = SyntheticFrameMeasurementPreamble::read_from_payload_prefix(&payload_buffer)
            .expect("preamble parses");
        assert_eq!(parsed.frame_sequence_number, 4242);
        assert_eq!(parsed.source_emit_monotonic_nanoseconds, 987_654_321);
        assert_eq!(parsed.stage_callback_nanoseconds, 3_500_000);
        assert!(
            payload_buffer[SYNTHETIC_FRAME_MEASUREMENT_PREAMBLE_BYTES..]
                .iter()
                .all(|&byte| byte == 0xCD),
            "the stage patch overran into the pixel payload"
        );
    }

    /// A short buffer must be refused rather than silently truncated — a
    /// truncated preamble would deserialize into a plausible-looking but wrong
    /// sequence number and corrupt the drop accounting.
    #[test]
    fn preamble_refuses_a_buffer_shorter_than_the_contract() {
        let mut too_short = vec![0u8; SYNTHETIC_FRAME_MEASUREMENT_PREAMBLE_BYTES - 1];
        let preamble = SyntheticFrameMeasurementPreamble {
            frame_sequence_number: 1,
            source_emit_monotonic_nanoseconds: 2,
            stage_callback_nanoseconds: 3,
        };
        assert!(!preamble.write_into_payload_prefix(&mut too_short));
        assert!(
            !SyntheticFrameMeasurementPreamble::patch_stage_callback_nanoseconds_in_payload_prefix(
                &mut too_short,
                1,
            )
        );
        assert!(SyntheticFrameMeasurementPreamble::read_from_payload_prefix(&too_short).is_none());
    }

    /// Little-endian is the stated contract; pin the exact byte order so a
    /// future edit that swaps endianness fails here rather than silently
    /// producing nonsense stamps.
    #[test]
    fn preamble_byte_order_is_little_endian() {
        let preamble = SyntheticFrameMeasurementPreamble {
            frame_sequence_number: 1,
            source_emit_monotonic_nanoseconds: 1,
            stage_callback_nanoseconds: 1,
        };
        let mut payload_buffer = vec![0u8; SYNTHETIC_FRAME_MEASUREMENT_PREAMBLE_BYTES];
        assert!(preamble.write_into_payload_prefix(&mut payload_buffer));
        assert_eq!(
            payload_buffer,
            vec![1, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0]
        );
    }
}
