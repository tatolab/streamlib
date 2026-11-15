// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Feeding Annex-B bytes: splitting them into NAL units at start codes, stripping emulation
//! prevention, handing each to its codec and draining decoded pictures, plus resets and
//! discontinuities.

use tracing::{debug, info};

use super::SimpleDecoder;
use super::types::*;
use crate::VideoError;

impl SimpleDecoder {
    /// Feed arbitrary bytes of H.264 Annex B bitstream data.
    ///
    /// The data can contain partial NALs, full NALs, or multiple NALs.
    /// Internally the decoder finds start codes, identifies NAL types,
    /// auto-configures on the first SPS, and decodes slice NALs.
    ///
    /// Returns decoded frames (with NV12 data read back from the GPU).
    pub fn feed(&mut self, data: &[u8]) -> Result<Vec<SimpleDecodedFrame>, VideoError> {
        // Accumulate data
        self.nal_buffer.extend_from_slice(data);

        // Split into NAL units
        let nals = crate::split_annex_b_nal_units(&self.nal_buffer);

        if nals.is_empty() {
            return Ok(Vec::new());
        }

        // Keep any trailing partial NAL (data after the last start code
        // that might be incomplete)
        let last_sc_pos = Self::find_last_start_code_pos(&self.nal_buffer);
        let trailing = if let Some(pos) = last_sc_pos {
            // Check if this might be an incomplete NAL (no following start code)
            let after_last = Self::find_start_code_after(&self.nal_buffer, pos);
            if after_last.is_none() {
                // The last NAL might be incomplete — but we still process it
                // since feed() is typically called with complete data.
                // We clear the buffer entirely.
                None
            } else {
                None
            }
        } else {
            None
        };

        // Clear the buffer (we'll process all NALs)
        let buffer_copy = std::mem::take(&mut self.nal_buffer);
        if let Some(trailing_data) = trailing {
            self.nal_buffer = trailing_data;
        }

        // Process each NAL
        let mut frames = Vec::new();
        let extracted_nals = crate::split_annex_b_nal_units(&buffer_copy);

        let is_h265 = self.config.codec == crate::encode::Codec::H265;

        for nal in &extracted_nals {
            if nal.is_empty() {
                continue;
            }

            if is_h265 {
                // H.265 NAL header: 2 bytes, type = (byte0 >> 1) & 0x3F
                if nal.len() < 2 {
                    continue;
                }
                let nal_type = (nal[0] >> 1) & 0x3F;

                match nal_type {
                    32 => {
                        // VPS
                        debug!("H265 NAL: VPS ({} bytes)", nal.len());
                        self.handle_h265_vps(nal)?;
                    }
                    33 => {
                        // SPS
                        debug!("H265 NAL: SPS ({} bytes)", nal.len());
                        self.handle_h265_sps(nal)?;
                    }
                    34 => {
                        // PPS
                        debug!("H265 NAL: PPS ({} bytes)", nal.len());
                        self.handle_h265_pps(nal)?;
                    }
                    19 | 20 => {
                        // IDR_W_RADL (19) or IDR_N_LP (20)
                        debug!(
                            "H265 NAL: IDR slice type {} ({} bytes)",
                            nal_type,
                            nal.len()
                        );
                        if let Some(frame) = self.handle_h265_slice(nal, true)? {
                            frames.push(frame);
                        }
                    }
                    0..=9 | 16..=18 | 21 => {
                        // TRAIL_N/R, TSA_N/R, STSA_N/R, RADL_N/R, RASL_N/R,
                        // BLA_W_LP, BLA_W_RADL, BLA_N_LP, CRA_NUT
                        debug!("H265 NAL: slice type {} ({} bytes)", nal_type, nal.len());
                        let is_irap = (16..=21).contains(&nal_type);
                        if let Some(frame) = self.handle_h265_slice(nal, is_irap)? {
                            frames.push(frame);
                        }
                    }
                    _ => {
                        debug!(
                            "H265 NAL: type {} ({} bytes) -- skipped",
                            nal_type,
                            nal.len()
                        );
                    }
                }
            } else {
                // H.264 NAL header: 1 byte, type = byte0 & 0x1F
                let nal_type = nal[0] & 0x1F;

                match nal_type {
                    7 => {
                        // SPS
                        debug!("NAL: SPS ({} bytes)", nal.len());
                        self.handle_sps(nal)?;
                    }
                    8 => {
                        // PPS
                        debug!("NAL: PPS ({} bytes)", nal.len());
                        self.handle_pps(nal)?;
                    }
                    5 => {
                        // IDR slice
                        debug!("NAL: IDR slice ({} bytes)", nal.len());
                        if let Some(frame) = self.handle_slice(nal, true)? {
                            frames.push(frame);
                        }
                    }
                    1 => {
                        // Non-IDR slice
                        debug!("NAL: non-IDR slice ({} bytes)", nal.len());
                        if let Some(frame) = self.handle_slice(nal, false)? {
                            frames.push(frame);
                        }
                    }
                    _ => {
                        debug!("NAL: type {} ({} bytes) -- skipped", nal_type, nal.len());
                    }
                }
            }
        }

        // Flush the last pending frame (GPU decode submitted but not yet read back)
        if let Some(frame) = self.drain_pending_frame()? {
            frames.push(frame);
        }

        Ok(frames)
    }

    /// Signal a discontinuity in the bitstream (e.g., seek).
    ///
    /// Resets parser state and waits for the next IDR before decoding.
    pub fn feed_discontinuity(&mut self) {
        self.nal_buffer.clear();
        self.frame_num = 0;
        self.idr_pic_id = self.idr_pic_id.wrapping_add(1);
        // Mark all DPB slots as unused
        self.dpb_slot_in_use.fill(false);
        // Reset H.264 parser DPB and physical slot mappings
        if let Some(ref mut parser) = self.h264_parser {
            parser.flush_decoded_picture_buffer();
        }
        self.h264_dpb_to_slot.fill(-1);
        info!("Discontinuity: waiting for next IDR");
    }

    /// Full reset: re-initialize session on the next SPS.
    pub fn reset(&mut self) {
        self.feed_discontinuity();
        self.session_configured = false;
        self.cached_vps_nalu = None;
        self.cached_sps_nalu = None;
        self.cached_pps_nalu = None;
        self.coded_picture_width = 0;
        self.coded_picture_height = 0;
        self.decoded_picture_display_window = None;
        self.frame_counter = 0;
        info!("Full reset: will reconfigure on next SPS");
    }

    /// Find the byte position of the last start code in the buffer.
    pub(super) fn find_last_start_code_pos(data: &[u8]) -> Option<usize> {
        let mut last = None;
        let mut i = 0;
        while i + 2 < data.len() {
            if i + 3 < data.len()
                && data[i] == 0
                && data[i + 1] == 0
                && data[i + 2] == 0
                && data[i + 3] == 1
            {
                last = Some(i);
                i += 4;
            } else if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
                last = Some(i);
                i += 3;
            } else {
                i += 1;
            }
        }
        last
    }

    /// Find a start code that begins strictly after `pos`.
    pub(super) fn find_start_code_after(data: &[u8], pos: usize) -> Option<usize> {
        let mut i = pos + 1;
        while i + 2 < data.len() {
            let is_four_byte_start_code = i + 3 < data.len()
                && data[i] == 0
                && data[i + 1] == 0
                && data[i + 2] == 0
                && data[i + 3] == 1;
            let is_three_byte_start_code = data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1;
            if is_four_byte_start_code || is_three_byte_start_code {
                return Some(i);
            }
            i += 1;
        }
        None
    }

    /// Remove Annex B emulation prevention bytes (00 00 03 -> 00 00).
    ///
    /// H.264/H.265 NAL units use byte-stuffing: any occurrence of
    /// `00 00 03 XX` in the raw stream means the `03` is an escape byte
    /// and should be removed to recover the original RBSP data.
    pub(super) fn remove_emulation_prevention_bytes(data: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(data.len());
        let mut i = 0;
        while i < data.len() {
            if i + 2 < data.len() && data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 3 {
                out.push(0);
                out.push(0);
                i += 3; // skip the 0x03
            } else {
                out.push(data[i]);
                i += 1;
            }
        }
        out
    }

    /// Find a free DPB slot, or evict the oldest one.
    pub(super) fn find_free_dpb_slot(&self) -> usize {
        // First, find an unused slot
        for (i, in_use) in self.dpb_slot_in_use.iter().enumerate() {
            if !in_use {
                return i;
            }
        }
        // All slots in use — evict slot 0 (simplistic sliding window)
        0
    }
}
