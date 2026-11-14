// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: GPL-3.0-only

//! H.264 and H.265 bitstream reading with no GPU: Annex-B access units and
//! their length-prefixed samples, start codes, RBSP emulation prevention, and
//! the sequence parameter sets a decoder sizes itself from.

pub mod annex_b_access_unit;
pub mod annex_b_start_code_finder;
pub mod h264_sequence_parameter_set;
pub mod h265_sequence_parameter_set;
pub mod nal_unit_raw_byte_sequence_payload;

/// The elementary stream a codec session encodes or decodes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoCodecElementaryStream {
    /// H.264 / AVC.
    H264,
    /// H.265 / HEVC.
    H265,
}
