// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! H.265-specific NAL handling: VPS/SPS/PPS parsing, slice decode submission.

use vulkanalia::vk;
use tracing::{debug, warn};

use crate::nv_video_parser::vulkan_h265_decoder::{
    self as h265dec, BitstreamReader as H265BitstreamReader,
    NalUnitType as H265NalUnitType, VulkanH265Decoder,
    HEVC_DPB_SIZE, MAX_NUM_PPS, MAX_NUM_SPS, MAX_NUM_VPS,
};
use crate::video_context::VideoError;

use super::{SimpleDecoder, PendingFrame};
use super::types::*;

impl SimpleDecoder {
    // ------------------------------------------------------------------
    // H.265 NAL handlers
    // ------------------------------------------------------------------

    pub(crate) fn handle_h265_vps(&mut self, vps_nalu: &[u8]) -> Result<(), VideoError> {
        self.cached_vps_nalu = Some(vps_nalu.to_vec());
        // Initialize the h265 parser if not yet created
        if self.h265_parser.is_none() {
            self.h265_parser = Some(VulkanH265Decoder::new());
        }

        // Parse VPS from RBSP data (after EPB removal, skip 2-byte NAL header)
        let rbsp = Self::remove_emulation_prevention_bytes(&vps_nalu[2..]);
        let mut reader = H265BitstreamReader::new(&rbsp);
        if let Some(vps) = VulkanH265Decoder::parse_vps(&mut reader) {
            let vps_id = vps.vps_video_parameter_set_id as usize;
            debug!(
                vps_id,
                max_sub_layers = vps.vps_max_sub_layers_minus1,
                "H265 VPS parsed"
            );
            let parser = self.h265_parser.as_mut().unwrap();
            if vps_id < MAX_NUM_VPS {
                let boxed = Box::new(vps);
                parser.active_vps = Some(boxed.clone());
                parser.vpss[vps_id] = Some(boxed);
            }
        } else {
            debug!("H265 VPS cached but parse failed ({} bytes)", vps_nalu.len());
        }
        Ok(())
    }

    pub(crate) fn handle_h265_sps(&mut self, sps_nalu: &[u8]) -> Result<(), VideoError> {
        self.cached_sps_nalu = Some(sps_nalu.to_vec());

        // Initialize the h265 parser if not yet created
        if self.h265_parser.is_none() {
            self.h265_parser = Some(VulkanH265Decoder::new());
        }

        // Remove emulation prevention bytes and skip 2-byte NAL header
        let rbsp = Self::remove_emulation_prevention_bytes(&sps_nalu[2..]);
        let mut reader = H265BitstreamReader::new(&rbsp);

        // Full SPS parse
        let sps = VulkanH265Decoder::parse_sps(&mut reader).ok_or_else(|| {
            VideoError::BitstreamError("Failed to parse H.265 SPS".into())
        })?;

        let width = sps.pic_width_in_luma_samples;
        let height = sps.pic_height_in_luma_samples;
        let sps_id = sps.sps_seq_parameter_set_id as usize;

        self.sps_width = width;
        self.sps_height = height;

        debug!(
            width,
            height,
            sps_id,
            log2_max_poc_lsb = sps.log2_max_pic_order_cnt_lsb_minus4 + 4,
            num_strps = sps.num_short_term_ref_pic_sets,
            max_dpb = sps.max_dec_pic_buffering,
            max_reorder = sps.max_num_reorder_pics,
            "H265 SPS parsed"
        );

        // Store in parser's SPS store and set as active
        let parser = self.h265_parser.as_mut().unwrap();
        if sps_id < MAX_NUM_SPS {
            // Set DPB sizing from parsed SPS
            parser.max_dec_pic_buffering = sps.max_dec_pic_buffering as i32;
            parser.max_dpb_size = VulkanH265Decoder::get_max_dpb_size(&sps);
            let boxed = Box::new(sps);
            parser.active_sps[0] = Some(boxed.clone());
            parser.spss[sps_id] = Some(boxed);
        }

        if !self.session_configured {
            self.configure_session()?;
        }

        Ok(())
    }

    pub(crate) fn handle_h265_pps(&mut self, pps_nalu: &[u8]) -> Result<(), VideoError> {
        self.cached_pps_nalu = Some(pps_nalu.to_vec());

        // Initialize the h265 parser if not yet created
        if self.h265_parser.is_none() {
            self.h265_parser = Some(VulkanH265Decoder::new());
        }

        let rbsp = Self::remove_emulation_prevention_bytes(&pps_nalu[2..]);
        let mut reader = H265BitstreamReader::new(&rbsp);

        let parser = self.h265_parser.as_mut().unwrap();
        let pps = VulkanH265Decoder::parse_pps(&mut reader, &parser.spss).ok_or_else(|| {
            VideoError::BitstreamError("Failed to parse H.265 PPS".into())
        })?;

        let pps_id = pps.pps_pic_parameter_set_id as usize;
        debug!(
            pps_id,
            init_qp = pps.init_qp_minus26 + 26,
            extra_bits = pps.num_extra_slice_header_bits,
            "H265 PPS parsed"
        );

        if pps_id < MAX_NUM_PPS {
            let boxed = Box::new(pps);
            parser.active_pps[0] = Some(boxed.clone());
            parser.ppss[pps_id] = Some(boxed);
        }

        // Recreate session parameters now that PPS is available
        if self.session_configured {
            self.create_session_params_h265()?;
        }

        Ok(())
    }

    pub(crate) fn handle_h265_slice(
        &mut self,
        nal: &[u8],
        is_irap: bool,
    ) -> Result<Option<SimpleDecodedFrame>, VideoError> {
        if !self.session_configured {
            warn!("H265 slice NAL received before session configured — skipping");
            return Ok(None);
        }

        // Drain the previous in-flight frame (wait for GPU, read staging buffer).
        // This must happen before decode_frame() which reuses the command buffer
        // and will overwrite the staging buffer.
        let prev_frame = self.drain_pending_frame()?;

        if self.h265_parser.is_none() {
            warn!("H265 slice before parser initialized — skipping");
            return Ok(prev_frame);
        }

        // H.265 NAL header: 2 bytes
        let nal_type = (nal[0] >> 1) & 0x3F;
        let nuh_temporal_id_plus1 = (nal[1] & 0x07) as u8;
        let is_idr = nal_type == H265NalUnitType::IdrWRadl as u8
            || nal_type == H265NalUnitType::IdrNLp as u8;

        // Get active SPS/PPS for slice header parsing
        let parser = self.h265_parser.as_ref().unwrap();
        let sps = parser.active_sps[0]
            .as_ref()
            .ok_or_else(|| VideoError::BitstreamError("No active H.265 SPS".into()))?
            .clone();
        let pps = parser.active_pps[0]
            .as_ref()
            .ok_or_else(|| VideoError::BitstreamError("No active H.265 PPS".into()))?
            .clone();

        // Parse slice header from RBSP data (after EPB removal, skip 2-byte NAL header)
        let rbsp = Self::remove_emulation_prevention_bytes(&nal[2..]);
        let mut reader = H265BitstreamReader::new(&rbsp);
        let slh = VulkanH265Decoder::parse_slice_header(
            &mut reader,
            nal_type,
            nuh_temporal_id_plus1,
            &sps,
            &pps,
        )
        .ok_or_else(|| {
            VideoError::BitstreamError("Failed to parse H.265 slice header".into())
        })?;

        // Mutable borrow for POC calculation, RPS derivation, and DPB management
        let parser = self.h265_parser.as_mut().unwrap();

        // Set IRAP flags for POC calculation
        if is_irap {
            parser.no_rasl_output_flag = is_idr;
        }

        // dpb_picture_start() internally calls picture_order_count() and
        // reference_picture_set() — do NOT call them separately or POC state
        // gets corrupted by the double call.
        parser.dpb_picture_start(&pps, &slh);

        // Read back the computed POC and RPS results
        let poc_val = parser.dpb_cur.map_or(0, |idx| parser.dpb[idx].pic_order_cnt_val);
        let num_poc_st_curr_before = parser.num_poc_st_curr_before;
        let num_poc_st_curr_after = parser.num_poc_st_curr_after;
        let num_poc_lt_curr = parser.num_poc_lt_curr;
        let num_delta_pocs = parser.num_delta_pocs_of_ref_rps_idx;

        debug!(
            poc = poc_val,
            slice_type = slh.slice_type,
            is_idr,
            num_st_before = num_poc_st_curr_before,
            num_st_after = num_poc_st_curr_after,
            num_lt = num_poc_lt_curr,
            rps_bits = slh.num_bits_for_short_term_rps_in_slice,
            "H265 slice"
        );
        // Extract parser state into fixed-size arrays (max HEVC DPB = 16)
        let cur_dpb_id = parser.current_dpb_id;
        let mut st_before = [(0i8, 0i32); 16];
        let nb_before = (num_poc_st_curr_before as usize).min(16);
        for i in 0..nb_before {
            let idx = parser.ref_pic_set_st_curr_before[i];
            let poc = if idx >= 0 && (idx as usize) < HEVC_DPB_SIZE {
                parser.dpb[idx as usize].pic_order_cnt_val
            } else { 0 };
            st_before[i] = (idx, poc);
        }
        let mut st_after = [(0i8, 0i32); 16];
        let nb_after = (num_poc_st_curr_after as usize).min(16);
        for i in 0..nb_after {
            let idx = parser.ref_pic_set_st_curr_after[i];
            let poc = if idx >= 0 && (idx as usize) < HEVC_DPB_SIZE {
                parser.dpb[idx as usize].pic_order_cnt_val
            } else { 0 };
            st_after[i] = (idx, poc);
        }
        let mut lt_curr_arr = [(0i8, 0i32); 16];
        let nb_lt = (num_poc_lt_curr as usize).min(16);
        for i in 0..nb_lt {
            let idx = parser.ref_pic_set_lt_curr[i];
            let poc = if idx >= 0 && (idx as usize) < HEVC_DPB_SIZE {
                parser.dpb[idx as usize].pic_order_cnt_val
            } else { 0 };
            lt_curr_arr[i] = (idx, poc);
        }

        // Build reference slot lists by mapping logical DPB → physical Vulkan slots
        let total_refs = nb_before + nb_after + nb_lt;
        let mut ref_slots = Vec::with_capacity(total_refs);
        let mut ref_pic_infos = Vec::with_capacity(total_refs);

        let mut skipped_refs = 0u32;
        for &(dpb_idx, poc) in &st_before[..nb_before] {
            if dpb_idx < 0 || dpb_idx as usize >= HEVC_DPB_SIZE {
                skipped_refs += 1;
                continue;
            }
            let slot = self.h265_dpb_to_slot[dpb_idx as usize];
            if slot < 0 {
                warn!(dpb_idx, poc, "st_before entry has no physical slot mapping!");
                skipped_refs += 1;
                continue;
            }
            let view_opt = self.vk_decoder.as_ref()
                .and_then(|vk_dec| vk_dec.dpb_slot_image_view(slot as usize));
            if let Some(view) = view_opt {
                ref_slots.push(ReferenceSlot {
                    slot_index: slot,
                    image_view: view,
                    image_layout: vk::ImageLayout::VIDEO_DECODE_DPB_KHR,
                });
                ref_pic_infos.push(H265RefPicInfo {
                    pic_order_cnt_val: poc,
                    long_term_ref: false,
                });
            }
        }
        let num_before = ref_slots.len();
        if skipped_refs > 0 {
            warn!(
                frame = self.frame_counter,
                skipped = skipped_refs,
                expected = num_poc_st_curr_before,
                actual = num_before,
                "H265 st_before: some references had no physical slot!"
            );
        }

        for &(dpb_idx, poc) in &st_after[..nb_after] {
            if dpb_idx < 0 || dpb_idx as usize >= HEVC_DPB_SIZE {
                continue;
            }
            let slot = self.h265_dpb_to_slot[dpb_idx as usize];
            if slot < 0 {
                continue;
            }
            let view_opt = self.vk_decoder.as_ref()
                .and_then(|vk_dec| vk_dec.dpb_slot_image_view(slot as usize));
            if let Some(view) = view_opt {
                ref_slots.push(ReferenceSlot {
                    slot_index: slot,
                    image_view: view,
                    image_layout: vk::ImageLayout::VIDEO_DECODE_DPB_KHR,
                });
                ref_pic_infos.push(H265RefPicInfo {
                    pic_order_cnt_val: poc,
                    long_term_ref: false,
                });
            }
        }
        let num_after = ref_slots.len() - num_before;

        for &(dpb_idx, poc) in &lt_curr_arr[..nb_lt] {
            if dpb_idx < 0 || dpb_idx as usize >= HEVC_DPB_SIZE {
                continue;
            }
            let slot = self.h265_dpb_to_slot[dpb_idx as usize];
            if slot < 0 {
                continue;
            }
            let view_opt = self.vk_decoder.as_ref()
                .and_then(|vk_dec| vk_dec.dpb_slot_image_view(slot as usize));
            if let Some(view) = view_opt {
                ref_slots.push(ReferenceSlot {
                    slot_index: slot,
                    image_view: view,
                    image_layout: vk::ImageLayout::VIDEO_DECODE_DPB_KHR,
                });
                ref_pic_infos.push(H265RefPicInfo {
                    pic_order_cnt_val: poc,
                    long_term_ref: true,
                });
            }
        }

        // Release the old physical slot BEFORE searching for a free slot.
        // When the parser reuses a logical DPB entry (e.g., entry 0 freed by
        // reference_picture_set, then reused for the current picture), the old
        // physical slot mapping must be released first so find_free_dpb_slot_h265
        // can reuse it. Without this, the search picks a fresh slot (e.g., slot 4)
        // instead of reusing the freed slot (slot 0), causing every-5th-frame
        // failures when the DPB cycles.
        if cur_dpb_id >= 0 && (cur_dpb_id as usize) < HEVC_DPB_SIZE {
            let old_slot = self.h265_dpb_to_slot[cur_dpb_id as usize];
            if old_slot >= 0 {
                let os = old_slot as usize;
                if os < self.dpb_slot_in_use.len() {
                    self.dpb_slot_in_use[os] = false;
                }
                self.h265_dpb_to_slot[cur_dpb_id as usize] = -1;
            }
        }

        // Allocate a physical Vulkan DPB slot for this picture.
        let setup_slot = self.find_free_dpb_slot_h265();

        // Map the current logical DPB entry to this physical slot.
        if cur_dpb_id >= 0 && (cur_dpb_id as usize) < HEVC_DPB_SIZE {
            self.h265_dpb_to_slot[cur_dpb_id as usize] = setup_slot as i32;
        }

        // Get DPB slot image/view and session params from VkVideoDecoder
        let vk_dec_ref = self.vk_decoder.as_ref().ok_or_else(|| {
            VideoError::BitstreamError("VkVideoDecoder not available for H.265 decode".into())
        })?;
        let setup_view = vk_dec_ref.dpb_slot_image_view(setup_slot)
            .ok_or_else(|| VideoError::BitstreamError(
                format!("VkVideoDecoder DPB slot {} not available", setup_slot),
            ))?;
        let setup_image = vk_dec_ref.dpb_image();
        let session_params = vk_dec_ref.session_parameters();

        // Diagnostic: log decode submission details for first 10 frames
        if self.frame_counter < 10 {
            let ref_info_str: Vec<String> = ref_slots.iter().zip(ref_pic_infos.iter())
                .map(|(s, r)| format!("slot{}(poc={})", s.slot_index, r.pic_order_cnt_val))
                .collect();
            debug!(
                frame = self.frame_counter,
                poc = poc_val,
                setup_slot,
                refs = ?ref_info_str,
                is_idr,
                num_delta_pocs,
                rps_bits = slh.num_bits_for_short_term_rps_in_slice,
                num_before,
                num_after = ref_slots.len() - num_before,
                bitstream_len = nal.len() + 3,
                nal_header = format!("{:02x} {:02x}", nal[0], nal[1]),
                nal_type = (nal[0] >> 1) & 0x3F,
                "H265 decode submit"
            );
        }

        // 3-byte start code per ffmpeg vulkan_decode.c and NVIDIA reference
        let mut bitstream_buf = vec![0x00, 0x00, 0x01];
        bitstream_buf.extend_from_slice(nal);
        let slice_offset = 0u32;

        // Diagnostic: dump first bytes for debugging
        if self.frame_counter < 5 {
            let hex: String = bitstream_buf[..bitstream_buf.len().min(20)]
                .iter().map(|b| format!("{:02x}", b)).collect::<Vec<_>>().join(" ");
            debug!(
                frame = self.frame_counter,
                total_len = bitstream_buf.len(),
                slice_offset,
                hex = %hex,
                "H265 bitstream"
            );
        }

        // Build active_slots list: ALL currently in-use DPB slots so the
        // Vulkan driver doesn't deactivate them when the coding scope ends.
        let mut active_slot_list = Vec::with_capacity(self.dpb_slot_in_use.len());
        for (i, &in_use) in self.dpb_slot_in_use.iter().enumerate() {
            if in_use && i != setup_slot {
                let view_opt = self.vk_decoder.as_ref()
                    .and_then(|vk_dec| vk_dec.dpb_slot_image_view(i));
                if let Some(view) = view_opt {
                    active_slot_list.push(ReferenceSlot {
                        slot_index: i as i32,
                        image_view: view,
                        image_layout: vk::ImageLayout::VIDEO_DECODE_DPB_KHR,
                    });
                }
            }
        }

        let submit = DecodeSubmitInfo {
            bitstream: &bitstream_buf,
            bitstream_offset: 0,
            setup_slot_index: setup_slot as i32,
            setup_image_view: setup_view,
            reference_slots: &ref_slots,
            active_slots: &active_slot_list,
            session_parameters: session_params,
            h264_info: None,
            h265_info: Some(H265DecodeInfo {
                pic_order_cnt_val: poc_val,
                vps_id: sps.sps_video_parameter_set_id,
                sps_id: sps.sps_seq_parameter_set_id,
                pps_id: pps.pps_pic_parameter_set_id,
                is_irap,
                is_idr,
                is_reference: true,
                slice_segment_offsets: vec![slice_offset],
                ref_pic_infos,
                setup_ref_info: H265RefPicInfo {
                    pic_order_cnt_val: poc_val,
                    long_term_ref: false,
                },
                num_delta_pocs_of_ref_rps_idx: num_delta_pocs as u8,
                num_bits_for_st_ref_pic_set_in_slice: slh.num_bits_for_short_term_rps_in_slice
                    as u16,
                num_st_curr_before: num_before as u8,
                num_st_curr_after: num_after as u8,
                short_term_ref_pic_set_sps_flag: slh.short_term_ref_pic_set_sps_flag != 0,
            }),
        };

        // Reuse persistent staging buffer for inline readback
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

        // Decode via VkVideoDecoder
        let vk_dec = self.vk_decoder.as_mut().ok_or_else(|| {
            VideoError::BitstreamError("VkVideoDecoder not available for H.265 decode".into())
        })?;
        unsafe {
            vk_dec.decode_frame(&submit, &mut output)?;
        }

        // End the logical DPB picture (marks entry as in-use/short-term)
        let parser = self.h265_parser.as_mut().unwrap();
        parser.dpb_picture_end();

        // Update physical DPB tracking
        self.dpb_slot_in_use[setup_slot] = true;
        self.dpb_slot_poc[setup_slot] = [poc_val, poc_val];

        // Aggressively release physical DPB slots that are no longer needed
        // as references. Keep only slots whose logical DPB entries are still
        // marked as short-term or long-term references.
        for i in 0..HEVC_DPB_SIZE {
            let phys_slot = self.h265_dpb_to_slot[i];
            if phys_slot >= 0 {
                let ps = phys_slot as usize;
                let should_release = parser.dpb[i].state == h265dec::DPB_STATE_EMPTY
                    || parser.dpb[i].marking == h265dec::DPB_MARKING_UNUSED;
                if should_release {
                    if ps < self.dpb_slot_in_use.len() {
                        self.dpb_slot_in_use[ps] = false;
                    }
                    self.h265_dpb_to_slot[i] = -1;
                }
            }
        }

        self.frame_num = self.frame_num.wrapping_add(1);

        // Store current frame as pending — GPU decode is in flight, staging
        // buffer will be read at the start of the next handle_h265_slice call
        // (or flush). This overlaps GPU decode with CPU prep for the next frame.
        self.pending_frame = Some(PendingFrame {
            width,
            height,
            decode_order: self.frame_counter,
            poc: poc_val,
            _setup_slot: setup_slot,
            _setup_image: setup_image,
        });

        self.frame_counter += 1;

        Ok(prev_frame)
    }

    /// Find a free physical Vulkan DPB slot, using logical DPB info for
    /// smarter eviction.
    fn find_free_dpb_slot_h265(&self) -> usize {
        // First: truly free slot
        for (i, in_use) in self.dpb_slot_in_use.iter().enumerate() {
            if !in_use {
                return i;
            }
        }

        // Second: slot whose logical DPB entry is unused (no longer needed as reference)
        if let Some(parser) = &self.h265_parser {
            let mut best_slot = None;
            let mut best_poc = i32::MAX;
            for i in 0..HEVC_DPB_SIZE {
                let phys = self.h265_dpb_to_slot[i];
                if phys >= 0 && (phys as usize) < self.dpb_slot_in_use.len() {
                    if parser.dpb[i].marking == h265dec::DPB_MARKING_UNUSED
                        || parser.dpb[i].state == h265dec::DPB_STATE_EMPTY
                    {
                        if parser.dpb[i].pic_order_cnt_val < best_poc {
                            best_poc = parser.dpb[i].pic_order_cnt_val;
                            best_slot = Some(phys as usize);
                        }
                    }
                }
            }
            if let Some(slot) = best_slot {
                return slot;
            }
        }

        // Last resort: evict slot 0
        0
    }

    // ------------------------------------------------------------------
    // H.265 SPS dimension parsing
    // ------------------------------------------------------------------

    /// Parse width and height from H.265 SPS NALU.
    ///
    /// H.265 SPS NAL structure (after 2-byte NAL header):
    ///   sps_video_parameter_set_id: u(4)
    ///   sps_max_sub_layers_minus1: u(3)
    ///   sps_temporal_id_nesting_flag: u(1)
    ///   profile_tier_level(...)
    ///   sps_seq_parameter_set_id: ue(v)
    ///   chroma_format_idc: ue(v)
    ///   [if chroma_format_idc == 3: separate_colour_plane_flag: u(1)]
    ///   pic_width_in_luma_samples: ue(v)
    ///   pic_height_in_luma_samples: ue(v)
    #[allow(dead_code)] // Utility for external callers
    pub(crate) fn parse_h265_sps_dimensions(sps_nalu: &[u8]) -> (u32, u32) {
        use crate::nv_video_parser::vulkan_h265_decoder::BitstreamReader;

        // Skip 2-byte NAL header
        if sps_nalu.len() < 6 {
            return (0, 0);
        }

        // Remove emulation prevention bytes before parsing.
        // The profile_tier_level section has many zero bytes (44 reserved
        // bits) that trigger EPB insertion (00 00 03 sequences).
        let rbsp = Self::remove_emulation_prevention_bytes(&sps_nalu[2..]);
        let mut r = BitstreamReader::new(&rbsp);

        let _vps_id = r.u(4);
        let max_sub_layers_minus1 = match r.u(3) {
            Some(v) => v,
            None => return (0, 0),
        };
        let _temporal_id_nesting = r.u(1);

        // Parse profile_tier_level(true, max_sub_layers_minus1)
        // Fixed part: 2 + 1 + 5 + 32 + 4 + 44 + 8 = 96 bits
        if r.u(2).is_none() { return (0, 0); } // general_profile_space
        if r.u(1).is_none() { return (0, 0); } // general_tier_flag
        if r.u(5).is_none() { return (0, 0); } // general_profile_idc
        if r.u(32).is_none() { return (0, 0); } // general_profile_compatibility_flag[32]
        // progressive, interlaced, non_packed, frame_only = 4 bits
        if r.u(4).is_none() { return (0, 0); }
        // 44 reserved zero bits (constraint flags)
        if r.u(32).is_none() { return (0, 0); }
        if r.u(12).is_none() { return (0, 0); }
        let _general_level_idc = r.u(8);

        // Sub-layer flags (if max_sub_layers_minus1 > 0)
        let mut sub_layer_profile_present = [false; 6];
        let mut sub_layer_level_present = [false; 6];
        for i in 0..max_sub_layers_minus1 as usize {
            sub_layer_profile_present[i] = r.u(1) == Some(1);
            sub_layer_level_present[i] = r.u(1) == Some(1);
        }
        if max_sub_layers_minus1 > 0 {
            for _ in max_sub_layers_minus1..8 {
                let _ = r.u(2); // reserved_zero_2bits
            }
        }
        for i in 0..max_sub_layers_minus1 as usize {
            if sub_layer_profile_present[i] {
                // sub_layer profile: 2+1+5+32+4+44+8 = 96 bits
                // But the constraint flags are 48 bits total (4+44), not separate.
                // Actually profile_tier_level for sub_layer:
                // sub_layer_profile_space(2), tier_flag(1), profile_idc(5),
                // compatibility_flags(32), progressive+interlaced+non_packed+frame_only(4),
                // 44 reserved bits, level_idc(8) -- but level is separate
                // total = 2+1+5+32+4+44 = 88 bits for profile part
                if r.u(32).is_none() { return (0, 0); }
                if r.u(32).is_none() { return (0, 0); }
                if r.u(24).is_none() { return (0, 0); }
            }
            if sub_layer_level_present[i] {
                let _ = r.u(8); // sub_layer_level_idc
            }
        }

        // sps_seq_parameter_set_id
        let _sps_id = r.ue();

        // chroma_format_idc
        let chroma_format_idc = match r.ue() {
            Some(v) => v,
            None => return (0, 0),
        };
        if chroma_format_idc == 3 {
            let _ = r.u(1); // separate_colour_plane_flag
        }

        // pic_width_in_luma_samples, pic_height_in_luma_samples
        let width = match r.ue() {
            Some(v) => v,
            None => return (0, 0),
        };
        let height = match r.ue() {
            Some(v) => v,
            None => return (0, 0),
        };

        (width, height)
    }

    /// Parse log2_diff_max_min_luma_coding_block_size from an H.265 SPS NALU.
    ///
    /// This determines the CTB (Coding Tree Block) size, which varies by
    /// encoder capability (32 or 64 on NVIDIA GPUs). Default 2 (CTB=32).
    #[allow(dead_code)] // Utility for external callers
    pub(crate) fn parse_h265_sps_ctb_log2_diff(sps_nalu: &[u8]) -> u8 {
        use crate::nv_video_parser::vulkan_h265_decoder::BitstreamReader;

        if sps_nalu.len() < 6 {
            return 2;
        }
        let rbsp = Self::remove_emulation_prevention_bytes(&sps_nalu[2..]);
        let mut r = BitstreamReader::new(&rbsp);

        // Same parsing as parse_h265_sps_dimensions up to width/height
        let _ = r.u(4); // vps_id
        let max_sub = match r.u(3) { Some(v) => v, None => return 2 };
        let _ = r.u(1); // temporal_id_nesting

        // profile_tier_level
        r.u(2); r.u(1); r.u(5); r.u(32); r.u(4); r.u(32); r.u(12); r.u(8);
        // Sub-layer flags
        let mut slp = [false; 6];
        let mut sll = [false; 6];
        for i in 0..max_sub as usize { slp[i] = r.u(1) == Some(1); sll[i] = r.u(1) == Some(1); }
        if max_sub > 0 { for _ in max_sub..8 { let _ = r.u(2); } }
        for i in 0..max_sub as usize {
            if slp[i] { r.u(32); r.u(32); r.u(24); }
            if sll[i] { r.u(8); }
        }

        let _ = r.ue(); // sps_id
        let chroma = r.ue().unwrap_or(1);
        if chroma == 3 { let _ = r.u(1); }
        let _ = r.ue(); // width
        let _ = r.ue(); // height
        let conf_win = r.u(1).unwrap_or(0);
        if conf_win != 0 { r.ue(); r.ue(); r.ue(); r.ue(); }
        let _ = r.ue(); // bd_luma
        let _ = r.ue(); // bd_chroma
        let _ = r.ue(); // log2_max_poc

        // sps_sub_layer_ordering_info_present_flag
        let sub_layer_ordering = r.u(1).unwrap_or(0);
        let loops = if sub_layer_ordering != 0 { max_sub + 1 } else { 1 };
        for _ in 0..loops {
            let _ = r.ue(); // max_dec_pic_buffering_minus1
            let _ = r.ue(); // max_num_reorder_pics
            let _ = r.ue(); // max_latency_increase_plus1
        }

        let _log2_min_cb_minus3 = r.ue().unwrap_or(0);
        let log2_diff = r.ue().unwrap_or(2);

        log2_diff as u8
    }
}
