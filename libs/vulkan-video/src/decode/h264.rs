// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! H.264-specific NAL handling: SPS/PPS parsing, slice decode submission.

use vulkanalia::vk;
use tracing::{debug, warn};

use crate::nv_video_parser::vulkan_h264_decoder::{
    self as h264dec, BitstreamReader as H264BitstreamReader,
    VulkanH264Decoder, MAX_DPB_SIZE as H264_MAX_DPB_SIZE, MAX_REFS as H264_MAX_REFS,
    reference_picture_list_initialization_p_frame,
};
use crate::nv_video_parser::vulkan_h26x_decoder::SliceType;
use crate::video_context::VideoError;

use super::{SimpleDecoder, PendingFrame};
use super::types::*;

impl SimpleDecoder {
    // ------------------------------------------------------------------
    // SPS handling
    // ------------------------------------------------------------------

    pub(crate) fn handle_sps(&mut self, sps_nalu: &[u8]) -> Result<(), VideoError> {
        // Initialize parser if not yet created
        if self.h264_parser.is_none() {
            self.h264_parser = Some(VulkanH264Decoder::new());
        }

        // Remove emulation prevention bytes and skip 1-byte NAL header
        let rbsp = Self::remove_emulation_prevention_bytes(&sps_nalu[1..]);
        let mut reader = H264BitstreamReader::new(&rbsp);

        let parser = self.h264_parser.as_mut().unwrap();
        let sps_id = parser.parse_sps(&mut reader).ok_or_else(|| {
            VideoError::BitstreamError("Failed to parse H.264 SPS".into())
        })?;

        let sps = parser.spss[sps_id as usize].as_ref().ok_or_else(|| {
            VideoError::BitstreamError(format!("SPS {} not found after parse", sps_id))
        })?;

        // Extract dimensions from parsed SPS
        let frame_mbs_only = sps.flags.frame_mbs_only_flag;
        let mut width = (sps.pic_width_in_mbs_minus1 + 1) as u32 * 16;
        let mut height = (if frame_mbs_only { 1 } else { 2 })
            * (sps.pic_height_in_map_units_minus1 + 1) as u32
            * 16;

        // Apply frame cropping
        if sps.flags.frame_cropping_flag {
            let crop_unit_x: u32 = 2;
            let crop_unit_y: u32 = 2 * if frame_mbs_only { 1 } else { 2 };
            width -= crop_unit_x
                * (sps.frame_crop_left_offset as u32 + sps.frame_crop_right_offset as u32);
            height -= crop_unit_y
                * (sps.frame_crop_top_offset as u32 + sps.frame_crop_bottom_offset as u32);
        }

        // Set as active SPS
        parser.sps = Some(sps.clone());

        self.cached_sps_nalu = Some(sps_nalu.to_vec());
        self.sps_width = width;
        self.sps_height = height;

        debug!(width, height, sps_id, profile_idc = sps.profile_idc,
              level_idc = ?sps.level_idc,
              max_ref_frames = sps.max_num_ref_frames,
              poc_type = ?sps.pic_order_cnt_type,
              log2_max_frame_num_m4 = sps.log2_max_frame_num_minus4,
              chroma_format_idc = sps.chroma_format_idc,
              "H.264 SPS parsed");

        // Don't configure yet — wait for PPS to also be available.
        // Session will be configured in handle_pps or handle_slice.
        Ok(())
    }

    /// Parse width and height from H.264 SPS NALU.
    /// Uses the same logic as pipeline_test.rs parse_sps_info().
    #[allow(dead_code)] // Utility for external callers
    pub(crate) fn parse_sps_dimensions(sps_nalu: &[u8]) -> (u32, u32) {
        use crate::nv_video_parser::vulkan_h264_decoder::BitstreamReader;

        if sps_nalu.len() < 5 {
            return (0, 0);
        }

        let mut r = BitstreamReader::new(&sps_nalu[1..]);
        let profile_idc = r.u(8) as u32;
        let _constraint_flags = r.u(8);
        let _level_idc = r.u(8);
        let _sps_id = r.ue();

        // For high profile, skip chroma/scaling params
        if profile_idc == 100 || profile_idc == 110 || profile_idc == 122 || profile_idc == 244 {
            let chroma_format_idc = r.ue();
            if chroma_format_idc == 3 {
                let _ = r.u(1);
            }
            let _ = r.ue(); // bit_depth_luma_minus8
            let _ = r.ue(); // bit_depth_chroma_minus8
            let _ = r.u(1); // qpprime_y_zero
            let scaling_matrix_present = r.u(1);
            if scaling_matrix_present != 0 {
                let limit = if chroma_format_idc != 3 { 8 } else { 12 };
                for _ in 0..limit {
                    let present = r.u(1);
                    if present != 0 {
                        let size: i32 = if limit <= 6 { 16 } else { 64 };
                        let mut last_scale: i32 = 8;
                        let mut next_scale: i32 = 8;
                        for _ in 0..size {
                            if next_scale != 0 {
                                let delta = r.se();
                                next_scale = (last_scale + delta + 256) % 256;
                            }
                            last_scale = if next_scale == 0 { last_scale } else { next_scale };
                        }
                    }
                }
            }
        }

        let _log2_max_frame_num = r.ue();
        let pic_order_cnt_type = r.ue();
        if pic_order_cnt_type == 0 {
            let _ = r.ue();
        } else if pic_order_cnt_type == 1 {
            let _ = r.u(1);
            let _ = r.se();
            let _ = r.se();
            let n = r.ue();
            for _ in 0..n {
                let _ = r.se();
            }
        }
        let _ = r.ue(); // max_num_ref_frames
        let _ = r.u(1); // gaps_in_frame_num
        let pic_width_in_mbs_minus1 = r.ue() as u32;
        let pic_height_in_map_units_minus1 = r.ue() as u32;
        let frame_mbs_only_flag = r.u(1) as u32;

        let mut width = (pic_width_in_mbs_minus1 + 1) * 16;
        let mut height = (2 - frame_mbs_only_flag) * (pic_height_in_map_units_minus1 + 1) * 16;

        if frame_mbs_only_flag == 0 {
            let _ = r.u(1);
        }
        let _ = r.u(1); // direct_8x8
        let frame_cropping_flag = r.u(1);
        if frame_cropping_flag != 0 {
            let crop_left = r.ue() as u32;
            let crop_right = r.ue() as u32;
            let crop_top = r.ue() as u32;
            let crop_bottom = r.ue() as u32;
            let crop_unit_x: u32 = 2;
            let crop_unit_y: u32 = 2 * (2 - frame_mbs_only_flag);
            width -= crop_unit_x * (crop_left + crop_right);
            height -= crop_unit_y * (crop_top + crop_bottom);
        }

        (width, height)
    }

    // ------------------------------------------------------------------
    // PPS handling
    // ------------------------------------------------------------------

    pub(crate) fn handle_pps(&mut self, pps_nalu: &[u8]) -> Result<(), VideoError> {
        self.cached_pps_nalu = Some(pps_nalu.to_vec());

        if let Some(ref mut parser) = self.h264_parser {
            let rbsp = Self::remove_emulation_prevention_bytes(&pps_nalu[1..]);
            let mut reader = H264BitstreamReader::new(&rbsp);
            if !parser.parse_pps(&mut reader) {
                warn!("Failed to parse H.264 PPS");
            } else {
                // Set active PPS
                let pps_id = parser.ppss.iter().position(|p| p.is_some());
                if let Some(id) = pps_id {
                    parser.pps = parser.ppss[id].clone();
                }
                debug!("H.264 PPS parsed ({} bytes)", pps_nalu.len());
            }
        } else {
            debug!("PPS cached ({} bytes), parser not yet initialized", pps_nalu.len());
        }

        // Auto-configure session now that both SPS and PPS are available
        if !self.session_configured && self.sps_width > 0 && self.sps_height > 0 {
            if let Some(ref parser) = self.h264_parser {
                if parser.sps.is_some() && parser.ppss.iter().any(|p| p.is_some()) {
                    self.configure_session()?;
                }
            }
        }

        Ok(())
    }

    // ------------------------------------------------------------------
    // Slice handling
    // ------------------------------------------------------------------

    pub(crate) fn handle_slice(
        &mut self,
        nal: &[u8],
        is_idr: bool,
    ) -> Result<Option<SimpleDecodedFrame>, VideoError> {
        if !self.session_configured {
            warn!("Slice NAL received before session configured — skipping");
            return Ok(None);
        }

        // Drain previous pending frame (like H.265 path)
        let prev_frame = self.drain_pending_frame()?;

        let parser = self.h264_parser.as_mut().ok_or_else(|| {
            VideoError::BitstreamError("H.264 parser not initialized".into())
        })?;

        // Extract NAL header fields
        let nal_ref_idc = (nal[0] >> 5) & 0x3;
        let nal_unit_type = nal[0] & 0x1F;

        // On IDR, reset physical DPB slot mappings. The parser's
        // decoded_reference_picture_marking() and picture_order_count()
        // handle IDR-specific state resets internally (marks all refs unused,
        // resets POC MSB/LSB to 0).
        if is_idr {
            parser.flush_decoded_picture_buffer();
            for s in &mut self.h264_dpb_to_slot {
                *s = -1;
            }
            for s in &mut self.dpb_slot_in_use {
                *s = false;
            }
        }

        // Remove emulation prevention bytes and skip 1-byte NAL header
        let rbsp = Self::remove_emulation_prevention_bytes(&nal[1..]);
        let mut reader = H264BitstreamReader::new(&rbsp);

        // Parse slice header using the ported parser
        let slh = parser.parse_slice_header(&mut reader, nal_ref_idc, nal_unit_type)
            .ok_or_else(|| {
                VideoError::BitstreamError("Failed to parse H.264 slice header".into())
            })?;

        // Get SPS/PPS from the parser's arrays using the IDs from the slice header
        let pps_id = slh.pic_parameter_set_id as usize;
        let pps = parser.ppss.get(pps_id).and_then(|p| p.clone()).ok_or_else(|| {
            VideoError::BitstreamError(format!("H.264 PPS {} not found", pps_id))
        })?;
        let sps_id = pps.seq_parameter_set_id as usize;
        let sps = parser.spss.get(sps_id).and_then(|s| s.clone()).ok_or_else(|| {
            VideoError::BitstreamError(format!("H.264 SPS {} not found", sps_id))
        })?;
        // Update active SPS/PPS for POC calculation
        parser.sps = Some(sps.clone());
        parser.pps = Some(pps.clone());

        // Find an empty DPB slot for the current picture
        let mut cur_dpb_idx = H264_MAX_DPB_SIZE; // fallback: extra slot
        for i in 0..H264_MAX_DPB_SIZE {
            if parser.dpb[i].state == 0 {
                cur_dpb_idx = i;
                break;
            }
        }
        parser.i_cur = cur_dpb_idx;

        // Initialize current DPB entry
        parser.dpb[cur_dpb_idx] = h264dec::DpbEntry::default();
        parser.dpb[cur_dpb_idx].state = 3; // frame
        parser.dpb[cur_dpb_idx].frame_num = slh.frame_num;
        parser.dpb[cur_dpb_idx].not_existing = false;

        // Compute POC using the ported parser (handles all 3 POC types)
        parser.picture_order_count(&sps, &slh);
        let poc = [
            parser.dpb[cur_dpb_idx].top_field_order_cnt,
            parser.dpb[cur_dpb_idx].bottom_field_order_cnt,
        ];

        // Compute picture numbers for reference list construction
        let max_frame_num = 1 << (sps.log2_max_frame_num_minus4 + 4);
        parser.picture_numbers(&slh, max_frame_num);

        // Build reference picture lists using the ported parser
        let mut ref_pic_list0 = [0i8; H264_MAX_REFS];
        let slice_type = slh.slice_type;
        let num_refs_l0 = if slice_type == SliceType::P || slice_type == SliceType::B {
            reference_picture_list_initialization_p_frame(&parser.dpb, &mut ref_pic_list0)
        } else {
            0
        };

        // Limit active references to what the slice header declares
        let max_l0 = if slice_type == SliceType::P || slice_type == SliceType::B {
            (slh.num_ref_idx_l0_active_minus1 + 1) as usize
        } else {
            0
        };
        let num_refs = num_refs_l0.min(max_l0);

        // Map parser DPB references → physical Vulkan DPB slots
        let mut ref_slots = Vec::with_capacity(num_refs);
        let mut ref_pic_infos = Vec::with_capacity(num_refs);
        for i in 0..num_refs {
            let dpb_idx = ref_pic_list0[i] as usize;
            if dpb_idx > H264_MAX_DPB_SIZE {
                continue;
            }
            let phys_slot = self.h264_dpb_to_slot[dpb_idx];
            if phys_slot < 0 {
                warn!(dpb_idx, frame = self.frame_counter,
                      "H.264 ref list entry has no physical slot mapping");
                continue;
            }
            let ps = phys_slot as usize;
            let vk_dec = self.vk_decoder.as_ref().ok_or_else(|| {
                VideoError::BitstreamError("VkVideoDecoder not initialized for H.264".into())
            })?;
            let view_opt = vk_dec.dpb_slot_image_view(ps);
            if let Some(view) = view_opt {
                let entry = &parser.dpb[dpb_idx];
                ref_slots.push(ReferenceSlot {
                    slot_index: phys_slot,
                    image_view: view,
                    image_layout: vk::ImageLayout::VIDEO_DECODE_DPB_KHR,
                });
                ref_pic_infos.push(H264RefPicInfo {
                    frame_num: entry.frame_num as u16,
                    pic_order_cnt: [entry.top_field_order_cnt, entry.bottom_field_order_cnt],
                    long_term_ref: entry.top_field_marking != 0 && entry.top_field_marking == 2,
                    non_existing: entry.not_existing,
                });
            }
        }

        // Release old physical slot if parser reuses this logical DPB entry
        if cur_dpb_idx <= H264_MAX_DPB_SIZE {
            let old_slot = self.h264_dpb_to_slot[cur_dpb_idx];
            if old_slot >= 0 {
                let os = old_slot as usize;
                if os < self.dpb_slot_in_use.len() {
                    self.dpb_slot_in_use[os] = false;
                }
                self.h264_dpb_to_slot[cur_dpb_idx] = -1;
            }
        }

        // Allocate physical DPB slot for current picture
        let setup_slot = self.find_free_dpb_slot();
        self.h264_dpb_to_slot[cur_dpb_idx] = setup_slot as i32;

        // Get DPB slot image/view and session params from VkVideoDecoder
        let vk_dec = self.vk_decoder.as_ref().ok_or_else(|| {
            VideoError::BitstreamError("VkVideoDecoder not initialized for H.264".into())
        })?;
        let setup_view = vk_dec.dpb_slot_image_view(setup_slot)
            .ok_or_else(|| VideoError::BitstreamError(
                format!("VkVideoDecoder DPB slot {} not available", setup_slot),
            ))?;
        let setup_image = vk_dec.dpb_image();
        let session_params = vk_dec.session_parameters();

        // Prepend 4-byte Annex B start code (NVIDIA H.264 driver expects 4-byte)
        let mut nal_with_sc = vec![0x00, 0x00, 0x00, 0x01];
        nal_with_sc.extend_from_slice(nal);

        // Build active_slots list (all in-use DPB slots except setup)
        let vk_dec = self.vk_decoder.as_ref().ok_or_else(|| {
            VideoError::BitstreamError("VkVideoDecoder not initialized for H.264".into())
        })?;
        let mut active_slot_list = Vec::with_capacity(self.dpb_slot_in_use.len());
        for (i, &in_use) in self.dpb_slot_in_use.iter().enumerate() {
            if in_use && i != setup_slot {
                let view_opt = vk_dec.dpb_slot_image_view(i);
                if let Some(view) = view_opt {
                    active_slot_list.push(ReferenceSlot {
                        slot_index: i as i32,
                        image_view: view,
                        image_layout: vk::ImageLayout::VIDEO_DECODE_DPB_KHR,
                    });
                }
            }
        }

        if self.frame_counter < 10 {
            let ref_info_str: Vec<String> = ref_slots.iter().zip(ref_pic_infos.iter())
                .map(|(s, r)| format!("slot{}(fn={},poc={})", s.slot_index, r.frame_num, r.pic_order_cnt[0]))
                .collect();
            debug!(
                frame = self.frame_counter,
                poc = poc[0],
                frame_num = slh.frame_num,
                setup_slot,
                cur_dpb_idx,
                refs = ?ref_info_str,
                is_idr,
                slice_type = ?slice_type,
                "H264 decode submit"
            );
        }

        let submit = DecodeSubmitInfo {
            bitstream: &nal_with_sc,
            bitstream_offset: 0,
            setup_slot_index: setup_slot as i32,
            setup_image_view: setup_view,
            reference_slots: &ref_slots,
            active_slots: &active_slot_list,
            session_parameters: session_params,
            h264_info: Some(H264DecodeInfo {
                frame_num: slh.frame_num as u16,
                idr_pic_id: slh.idr_pic_id as u16,
                pic_order_cnt: poc,
                sps_id: sps.seq_parameter_set_id as u8,
                pps_id: pps.pic_parameter_set_id,
                is_idr,
                is_intra: slh.slice_type == SliceType::I,
                is_reference: nal_ref_idc > 0,
                field_pic_flag: slh.field_pic_flag,
                bottom_field_flag: slh.bottom_field_flag,
                slice_offsets: vec![0],
                ref_pic_infos,
                setup_ref_info: H264RefPicInfo {
                    frame_num: slh.frame_num as u16,
                    pic_order_cnt: poc,
                    long_term_ref: slh.long_term_reference_flag,
                    non_existing: false,
                },
            }),
            h265_info: None,
        };

        // Use inline staging buffer for readback (same command buffer as decode)
        let width = self.sps_width;
        let height = self.sps_height;
        self.ensure_readback_staging(width, height)?;

        let &(stg_buf, stg_alloc, stg_size, stg_ptr) = self.readback_staging.as_ref().unwrap();
        let mut output = DecodedFrame {
            staging_buffer: Some(StagingBuffer {
                buffer: stg_buf,
                allocation: stg_alloc,
                size: stg_size,
                mapped_ptr: stg_ptr,
            }),
            ..DecodedFrame::default()
        };

        // Use VkVideoDecoder (ported code) for H.264 decode
        let vk_dec = self.vk_decoder.as_mut().ok_or_else(|| {
            VideoError::BitstreamError("VkVideoDecoder not initialized for H.264".into())
        })?;
        unsafe { vk_dec.decode_frame(&submit, &mut output)?; }

        // Apply decoded reference picture marking (MMCO / sliding window)
        let parser = self.h264_parser.as_mut().unwrap();
        parser.decoded_reference_picture_marking(&slh, sps.max_num_ref_frames);

        // Update physical DPB tracking
        self.dpb_slot_in_use[setup_slot] = true;
        self.dpb_slot_poc[setup_slot] = poc;

        // Release physical slots and free parser DPB entries no longer referenced.
        // Without resetting state to 0, unreferenced entries accumulate with
        // state=3, exhausting all 16 parser DPB slots.  Once full, every frame
        // is forced into the overflow slot (index 16), overwriting its predecessor
        // and breaking the reference chain — the root cause of the quality
        // collapse after frame 16.
        for i in 0..H264_MAX_DPB_SIZE {
            if i == cur_dpb_idx {
                continue;
            }
            let entry = &parser.dpb[i];
            let is_ref = entry.top_field_marking != 0 || entry.bottom_field_marking != 0;
            if !is_ref {
                let phys = self.h264_dpb_to_slot[i];
                if phys >= 0 {
                    let ps = phys as usize;
                    if ps < self.dpb_slot_in_use.len() {
                        self.dpb_slot_in_use[ps] = false;
                    }
                    self.h264_dpb_to_slot[i] = -1;
                }
                // Free the parser DPB slot so it can be reused by future frames
                parser.dpb[i].state = 0;
            }
        }

        // Store as pending frame — staging buffer has data after sync decode
        self.pending_frame = Some(PendingFrame {
            width,
            height,
            decode_order: self.frame_counter,
            poc: poc[0],
            _setup_slot: setup_slot,
            _setup_image: setup_image,
        });

        self.frame_counter += 1;

        Ok(prev_frame)
    }
}
