// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Port of VkEncoderH264.h
//!
//! H.264 encoder types and helpers.
//! In the original C++ this header simply re-exports EncoderConfigH264.
//! Here it serves as a lightweight re-export / type alias module for
//! H.264 encoder configuration used by VkVideoEncoderH264.

// The C++ file is a trivial header that includes VkEncoderConfigH264.h.
// In Rust, we re-export the relevant H.264 encoder configuration items
// from the config module (ported separately in batch 1).

/// H.264 encoder session parameter state — mirrors the VideoSessionParametersInfo
/// helper from the C++ EncoderH264State used in VkVideoEncoderH264.
#[derive(Debug, Clone, Default)]
pub struct EncoderH264State {
    // SPS
    pub log2_max_frame_num_minus4: u32,
    pub log2_max_pic_order_cnt_lsb_minus4: u32,
    pub pic_order_cnt_type: u32,
    pub max_num_ref_frames: u32,
    pub seq_parameter_set_id: u8,
    pub gaps_in_frame_num_value_allowed_flag: bool,

    // PPS
    pub pic_parameter_set_id: u8,
    pub num_ref_idx_l0_default_active_minus1: u8,
    pub num_ref_idx_l1_default_active_minus1: u8,

    // Rate control (H.264-specific)
    pub temporal_layer_count: u32,
}

impl EncoderH264State {
    pub fn new() -> Self {
        Self::default()
    }
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_encoder_h264_state_default() {
        let state = EncoderH264State::new();
        assert_eq!(state.log2_max_frame_num_minus4, 0);
        assert_eq!(state.seq_parameter_set_id, 0);
        assert_eq!(state.temporal_layer_count, 0);
    }
}
