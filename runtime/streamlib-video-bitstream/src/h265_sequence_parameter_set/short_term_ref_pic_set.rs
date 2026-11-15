// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The H.265 short-term reference picture set and its parser (ITU-T H.265 §7.3.7).

use crate::nal_unit_raw_byte_sequence_payload::RbspBitstreamReader;

use super::MAX_NUM_STRPS_ENTRIES;

/// `abs_delta_rps_minus1` is in `0..=2^15 - 1` (ITU-T H.265 §7.4.8).
const H265_HIGHEST_ABS_DELTA_RPS_MINUS1: u32 = (1 << 15) - 1;

// ---------------------------------------------------------------------------
// Short-term reference picture set
// ---------------------------------------------------------------------------

/// Short-term reference picture set, ported from C++ `short_term_ref_pic_set_s`.
#[derive(Debug, Clone, Default)]
pub struct ShortTermRefPicSet {
    pub num_negative_pics: u8,
    pub num_positive_pics: u8,
    pub inter_ref_pic_set_prediction_flag: u8,
    pub delta_idx_minus1: u8,
    pub used_by_curr_pic_s0: [u8; MAX_NUM_STRPS_ENTRIES],
    pub used_by_curr_pic_s1: [u8; MAX_NUM_STRPS_ENTRIES],
    pub delta_poc_s0: [i32; MAX_NUM_STRPS_ENTRIES],
    pub delta_poc_s1: [i32; MAX_NUM_STRPS_ENTRIES],
}

/// Std-video compatible short-term ref pic set (bitmask-based).
/// Corresponds to `StdVideoH265ShortTermRefPicSet` in Vulkan headers.
#[derive(Debug, Clone, Default)]
pub struct StdShortTermRefPicSet {
    pub flags: StdShortTermRefPicSetFlags,
    pub delta_idx_minus1: u32,
    pub use_delta_flag: u32,
    pub abs_delta_rps_minus1: u32,
    pub used_by_curr_pic_flag: u32,
    pub used_by_curr_pic_s0_flag: u32,
    pub used_by_curr_pic_s1_flag: u32,
    pub num_negative_pics: u32,
    pub num_positive_pics: u32,
    pub delta_poc_s0_minus1: [u16; MAX_NUM_STRPS_ENTRIES],
    pub delta_poc_s1_minus1: [i32; MAX_NUM_STRPS_ENTRIES],
}

#[derive(Debug, Clone, Default)]
pub struct StdShortTermRefPicSetFlags {
    pub inter_ref_pic_set_prediction_flag: bool,
    pub delta_rps_sign: bool,
}

// -----------------------------------------------------------------------
// Short-term ref pic set parsing
// -----------------------------------------------------------------------

/// Parse a short_term_ref_pic_set from the bitstream.
/// Corresponds to C++ `short_term_ref_pic_set`.
///
/// `reader` provides the bitstream reading methods (u, ue, se).
/// Returns `None` on parse error.
pub fn parse_h265_short_term_ref_pic_set(
    reader: &mut RbspBitstreamReader,
    std_strps: &mut StdShortTermRefPicSet,
    strps: &mut ShortTermRefPicSet,
    all_strpss: &[ShortTermRefPicSet],
    idx: usize,
    num_short_term_ref_pic_sets: usize,
) -> Option<()> {
    let inter_ref_pic_set_prediction_flag = if idx != 0 { reader.u(1)? } else { 0 };
    strps.inter_ref_pic_set_prediction_flag = inter_ref_pic_set_prediction_flag as u8;
    std_strps.flags.inter_ref_pic_set_prediction_flag = inter_ref_pic_set_prediction_flag != 0;

    if inter_ref_pic_set_prediction_flag != 0 {
        let mut used_by_curr_pic_flag = [0u8; MAX_NUM_STRPS_ENTRIES + 1];
        let mut use_delta_flag = [0u8; MAX_NUM_STRPS_ENTRIES + 1];

        let delta_idx_minus1 = if idx == num_short_term_ref_pic_sets {
            reader.ue()?
        } else {
            0
        };
        if delta_idx_minus1 >= idx as u32 {
            tracing::warn!(
                "Invalid delta_idx_minus1 ({} > {})",
                delta_idx_minus1,
                idx - 1
            );
            return None;
        }
        strps.delta_idx_minus1 = delta_idx_minus1 as u8;
        std_strps.delta_idx_minus1 = delta_idx_minus1;

        let delta_rps_sign = reader.u(1)?;
        std_strps.flags.delta_rps_sign = delta_rps_sign != 0;
        let abs_delta_rps_minus1 = reader.ue()?;
        if abs_delta_rps_minus1 > H265_HIGHEST_ABS_DELTA_RPS_MINUS1 {
            tracing::warn!("Invalid abs_delta_rps_minus1 ({})", abs_delta_rps_minus1);
            return None;
        }
        std_strps.abs_delta_rps_minus1 = abs_delta_rps_minus1;
        let abs_delta_rps_minus1 = abs_delta_rps_minus1 as i32;

        let delta_rps = (1 - 2 * delta_rps_sign as i32) * (abs_delta_rps_minus1 + 1);
        let r_idx = idx as i32 - (delta_idx_minus1 as i32 + 1);
        if r_idx < 0 || r_idx as usize >= all_strpss.len() {
            return None;
        }
        let rstrps = &all_strpss[r_idx as usize];

        let total = rstrps.num_negative_pics as usize + rstrps.num_positive_pics as usize;
        for j in 0..=total {
            if j > MAX_NUM_STRPS_ENTRIES {
                break;
            }
            used_by_curr_pic_flag[j] = reader.u(1)? as u8;
            if used_by_curr_pic_flag[j] != 0 {
                std_strps.used_by_curr_pic_flag |= 1 << j;
            }
            use_delta_flag[j] = if used_by_curr_pic_flag[j] == 0 {
                reader.u(1)? as u8
            } else {
                1
            };
            if use_delta_flag[j] != 0 {
                std_strps.use_delta_flag |= 1 << j;
            }
        }

        // Derive S0 (negative)
        {
            let mut i = 0usize;
            for j in (0..rstrps.num_positive_pics as usize).rev() {
                let d_poc = rstrps.delta_poc_s1[j] + delta_rps;
                if d_poc < 0 && use_delta_flag[rstrps.num_negative_pics as usize + j] != 0 {
                    if i >= MAX_NUM_STRPS_ENTRIES {
                        break;
                    }
                    strps.delta_poc_s0[i] = d_poc;
                    std_strps.delta_poc_s0_minus1[i] = d_poc as u16;
                    strps.used_by_curr_pic_s0[i] =
                        used_by_curr_pic_flag[rstrps.num_negative_pics as usize + j];
                    if strps.used_by_curr_pic_s0[i] != 0 {
                        std_strps.used_by_curr_pic_s0_flag |= 1 << i;
                    }
                    i += 1;
                }
            }
            if delta_rps < 0
                && use_delta_flag
                    [rstrps.num_negative_pics as usize + rstrps.num_positive_pics as usize]
                    != 0
                && i < MAX_NUM_STRPS_ENTRIES
            {
                strps.delta_poc_s0[i] = delta_rps;
                std_strps.delta_poc_s0_minus1[i] = delta_rps as u16;
                strps.used_by_curr_pic_s0[i] = used_by_curr_pic_flag
                    [rstrps.num_negative_pics as usize + rstrps.num_positive_pics as usize];
                if strps.used_by_curr_pic_s0[i] != 0 {
                    std_strps.used_by_curr_pic_s0_flag |= 1 << i;
                }
                i += 1;
            }
            for j in 0..rstrps.num_negative_pics as usize {
                let d_poc = rstrps.delta_poc_s0[j] + delta_rps;
                if d_poc < 0 && use_delta_flag[j] != 0 {
                    if i >= MAX_NUM_STRPS_ENTRIES {
                        break;
                    }
                    strps.delta_poc_s0[i] = d_poc;
                    std_strps.delta_poc_s0_minus1[i] = d_poc as u16;
                    strps.used_by_curr_pic_s0[i] = used_by_curr_pic_flag[j];
                    if strps.used_by_curr_pic_s0[i] != 0 {
                        std_strps.used_by_curr_pic_s0_flag |= 1 << i;
                    }
                    i += 1;
                }
            }
            strps.num_negative_pics = i as u8;
            std_strps.num_negative_pics = i as u32;
        }

        // Derive S1 (positive)
        {
            let mut i = 0usize;
            for j in (0..rstrps.num_negative_pics as usize).rev() {
                let d_poc = rstrps.delta_poc_s0[j] + delta_rps;
                if d_poc > 0 && use_delta_flag[j] != 0 {
                    if i >= MAX_NUM_STRPS_ENTRIES {
                        break;
                    }
                    strps.delta_poc_s1[i] = d_poc;
                    std_strps.delta_poc_s1_minus1[i] = d_poc;
                    strps.used_by_curr_pic_s1[i] = used_by_curr_pic_flag[j];
                    if strps.used_by_curr_pic_s1[i] != 0 {
                        std_strps.used_by_curr_pic_s1_flag |= 1 << i;
                    }
                    i += 1;
                }
            }
            if delta_rps > 0
                && use_delta_flag
                    [rstrps.num_negative_pics as usize + rstrps.num_positive_pics as usize]
                    != 0
                && i < MAX_NUM_STRPS_ENTRIES
            {
                strps.delta_poc_s1[i] = delta_rps;
                std_strps.delta_poc_s1_minus1[i] = delta_rps;
                strps.used_by_curr_pic_s1[i] = used_by_curr_pic_flag
                    [rstrps.num_negative_pics as usize + rstrps.num_positive_pics as usize];
                if strps.used_by_curr_pic_s1[i] != 0 {
                    std_strps.used_by_curr_pic_s1_flag |= 1 << i;
                }
                i += 1;
            }
            for j in 0..rstrps.num_positive_pics as usize {
                let d_poc = rstrps.delta_poc_s1[j] + delta_rps;
                if d_poc > 0 && use_delta_flag[rstrps.num_negative_pics as usize + j] != 0 {
                    if i >= MAX_NUM_STRPS_ENTRIES {
                        break;
                    }
                    strps.delta_poc_s1[i] = d_poc;
                    std_strps.delta_poc_s1_minus1[i] = d_poc;
                    strps.used_by_curr_pic_s1[i] =
                        used_by_curr_pic_flag[rstrps.num_negative_pics as usize + j];
                    if strps.used_by_curr_pic_s1[i] != 0 {
                        std_strps.used_by_curr_pic_s1_flag |= 1 << i;
                    }
                    i += 1;
                }
            }
            strps.num_positive_pics = i as u8;
            std_strps.num_positive_pics = i as u32;
        }

        if strps.num_negative_pics as usize + strps.num_positive_pics as usize
            > MAX_NUM_STRPS_ENTRIES
        {
            tracing::warn!(
                "Invalid NumNegativePics+NumPositivePics ({}+{})",
                strps.num_negative_pics,
                strps.num_positive_pics
            );
            return None;
        }
    } else {
        // Direct coding (no inter-prediction)
        let num_negative_pics = reader.ue()?;
        let num_positive_pics = reader.ue()?;
        if num_negative_pics as usize > MAX_NUM_STRPS_ENTRIES
            || num_positive_pics as usize > MAX_NUM_STRPS_ENTRIES
            || (num_negative_pics + num_positive_pics) as usize > MAX_NUM_STRPS_ENTRIES
        {
            tracing::warn!(
                "Invalid num_negative_pics+num_positive_pics ({}+{})",
                num_negative_pics,
                num_positive_pics
            );
            return None;
        }

        let mut delta_poc_s0_minus1 = [0i16; MAX_NUM_STRPS_ENTRIES];
        let mut used_by_curr_pic_s0_flag = [0u8; MAX_NUM_STRPS_ENTRIES];
        let mut delta_poc_s1_minus1 = [0i16; MAX_NUM_STRPS_ENTRIES];
        let mut used_by_curr_pic_s1_flag = [0u8; MAX_NUM_STRPS_ENTRIES];

        for i in 0..num_negative_pics as usize {
            delta_poc_s0_minus1[i] = reader.ue()? as i16;
            used_by_curr_pic_s0_flag[i] = reader.u(1)? as u8;
        }
        for i in 0..num_positive_pics as usize {
            delta_poc_s1_minus1[i] = reader.ue()? as i16;
            used_by_curr_pic_s1_flag[i] = reader.u(1)? as u8;
        }

        strps.num_negative_pics = num_negative_pics as u8;
        std_strps.num_negative_pics = num_negative_pics;
        strps.num_positive_pics = num_positive_pics as u8;
        std_strps.num_positive_pics = num_positive_pics;

        for i in 0..num_negative_pics as usize {
            strps.delta_poc_s0[i] = (if i == 0 { 0 } else { strps.delta_poc_s0[i - 1] })
                - (delta_poc_s0_minus1[i] as i32 + 1);
            std_strps.delta_poc_s0_minus1[i] = strps.delta_poc_s0[i] as u16;
            strps.used_by_curr_pic_s0[i] = used_by_curr_pic_s0_flag[i];
            if strps.used_by_curr_pic_s0[i] != 0 {
                std_strps.used_by_curr_pic_s0_flag |= 1 << i;
            }
        }
        for i in 0..num_positive_pics as usize {
            strps.delta_poc_s1[i] = (if i == 0 { 0 } else { strps.delta_poc_s1[i - 1] })
                + (delta_poc_s1_minus1[i] as i32 + 1);
            std_strps.delta_poc_s1_minus1[i] = strps.delta_poc_s1[i];
            strps.used_by_curr_pic_s1[i] = used_by_curr_pic_s1_flag[i];
            if strps.used_by_curr_pic_s1[i] != 0 {
                std_strps.used_by_curr_pic_s1_flag |= 1 << i;
            }
        }
    }

    Some(())
}
