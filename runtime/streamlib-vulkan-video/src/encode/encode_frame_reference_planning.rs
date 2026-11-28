// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Planning a frame's references before its encode is recorded: picture order counts, the H.264
//! and H.265 codec DPB pipelines, and the DPB slots the frame writes and reads; and recording
//! the frame in its slot once it is coded.

use vulkanalia::vk;

use crate::vk_video_encoder::vk_encoder_dpb_h264::PicInfoH264;
use crate::vk_video_encoder::vk_encoder_dpb_h265::{
    NO_REFERENCE_PICTURE_H265, RefPicSetH265, SetupRefListResult,
};

use super::SimpleEncoder;
use super::config::FrameType;

/// What [`SimpleEncoder::plan_the_frame_references`] settles for one frame before its commands
/// are recorded: picture order, the reference lists and the DPB slots they occupy.
pub(super) struct EncodeFrameReferencePlan {
    /// The H.264 `frame_num`: the codec DPB's on the H.264 path, the frame count on the inline
    /// path, and 0 for H.265.
    pub(super) h264_frame_num: u32,
    /// The H.264 picture order count.
    pub(super) h264_poc: i32,
    /// H.264 L0 reference slots (backward references).
    pub(super) h264_ref_slot_list: Vec<u8>,
    /// H.264 L1 reference slots (forward references, B-frames only).
    pub(super) h264_ref_slot_list_l1: Vec<u8>,
    /// The H.265 picture order count.
    pub(super) h265_poc: i32,
    /// What the H.265 DPB pipeline's `InitializeRPS` produced, when the codec is H.265.
    pub(super) h265_rps_result:
        Option<crate::vk_video_encoder::vk_encoder_dpb_h265::InitializeRpsResult>,
    /// The H.265 reference picture lists, when the codec is H.265.
    pub(super) h265_ref_list_result: Option<SetupRefListResult>,
    /// Whether the session encodes H.264.
    pub(super) is_h264: bool,
    /// Whether the session encodes H.265.
    pub(super) is_h265: bool,
    /// Whether the frame stays in the DPB as a reference (every type but B).
    pub(super) is_reference: bool,
    /// The DPB slot the encoded picture is written into.
    pub(super) setup_slot_index: usize,
    /// The DPB slots the frame references, the setup slot excluded.
    pub(super) reference_slot_indices: Vec<usize>,
}

impl SimpleEncoder {
    /// Settle the frame's picture order count, reference lists and DPB slots through the
    /// codec DPB when it is active, or the inline sliding window otherwise.
    pub(super) fn plan_the_frame_references(
        &mut self,
        frame_type: FrameType,
        is_idr: bool,
    ) -> EncodeFrameReferencePlan {
        // --- DPB-managed frame info ---
        // When the codec DPB is active, use it for POC, reference lists, and
        // DPB slot management instead of the inline logic.
        let h264_frame_num: u32;
        let h264_poc: i32;
        let h264_setup_slot: i8;
        let h264_ref_slot_list: Vec<u8>; // L0 (backward references)
        let h264_ref_slot_list_l1: Vec<u8>; // L1 (forward references, B-frames only)

        // H.265 DPB-managed variables
        let h265_setup_slot: i8;
        let h265_ref_slot_list: Vec<u8>; // L0 (backward references)
        let h265_poc: i32;
        // Results from the H.265 DPB pipeline (initialized to defaults for non-H.265)
        let h265_rps_result: Option<
            crate::vk_video_encoder::vk_encoder_dpb_h265::InitializeRpsResult,
        >;
        let _h265_ref_pic_set: RefPicSetH265;
        let h265_ref_list_result: Option<SetupRefListResult>;

        let is_h264 = self.codec_flag == vk::VideoCodecOperationFlagsKHR::ENCODE_H264;
        let is_h265 = self.codec_flag == vk::VideoCodecOperationFlagsKHR::ENCODE_H265;
        let is_reference = frame_type != FrameType::B;

        if is_h265 && let Some(enc) = self.h265_encoder.as_mut() {
            // H.265 DPB-managed path: full pipeline matching C++ VkVideoEncoderH265.cpp
            // Call order: ReferencePictureMarking -> InitializeRPS -> DpbPictureStart
            //   -> SetupReferencePictureListLx -> DpbPictureEnd
            let is_irap = is_idr || frame_type == FrameType::I;
            let no_output_of_prior_pics = is_idr && self.frame_count > 0;

            h265_poc = self.poc_counter as i32;

            tracing::debug!(
                frame_count = self.frame_count,
                poc = h265_poc,
                frame_type = frame_type.name(),
                is_irap = is_irap,
                is_idr = is_idr,
                no_output_of_prior_pics = no_output_of_prior_pics,
                "H265 encode_frame BEGIN"
            );

            let pic_type: u32 = match frame_type {
                FrameType::Idr => 0, // PIC_TYPE_IDR
                FrameType::I => 1,   // PIC_TYPE_I
                FrameType::P => 2,   // PIC_TYPE_P
                FrameType::B => 3,   // PIC_TYPE_B
            };

            let h265_log2_max_poc_lsb_m4 = enc.log2_max_pic_order_cnt_lsb_minus4;
            let dpb = &mut enc.dpb;

            // Step 1: Reference picture marking (sliding window, CRA handling)
            tracing::debug!(
                h265_poc = h265_poc,
                pic_type = pic_type,
                "H265 encode Step 1: reference_picture_marking"
            );
            dpb.reference_picture_marking(h265_poc, pic_type, false);

            // Step 2: Initialize RPS (builds STRPS from DPB state)
            // For IDR/no_output_of_prior_pics, pShortTermRefPicSet is null in C++,
            // but we still call initialize_rps which returns an empty set for IDR.
            tracing::debug!(
                no_output_of_prior_pics = no_output_of_prior_pics,
                poc_counter = self.poc_counter,
                "H265 encode Step 2: initialize_rps"
            );
            let rps_res = if !no_output_of_prior_pics {
                Some(dpb.initialize_rps(
                    &[], // no SPS STRPS to match against
                    pic_type,
                    self.poc_counter as u32,
                    0, // temporal_id
                    is_irap,
                    1, // num_ref_l0
                    0, // num_ref_l1 (no B-frames)
                ))
            } else {
                tracing::debug!("  Skipping initialize_rps (no_output_of_prior_pics)");
                None
            };
            if let Some(ref rps) = rps_res {
                tracing::debug!(
                    sps_flag = rps.short_term_ref_pic_set_sps_flag,
                    sps_idx = rps.short_term_ref_pic_set_idx,
                    num_negative = rps.short_term_ref_pic_set.num_negative_pics,
                    num_positive = rps.short_term_ref_pic_set.num_positive_pics,
                    "  Step 2 result: InitializeRpsResult"
                );
            }

            // Step 3: DpbPictureStart with RPS
            tracing::debug!("H265 encode Step 3: dpb_picture_start_with_rps");
            let strps_for_start = rps_res
                .as_ref()
                .map(|r| r.short_term_ref_pic_set.clone())
                .unwrap_or_default();
            let max_pic_order_cnt_lsb: i32 = 1 << (h265_log2_max_poc_lsb_m4 + 4);
            let (slot_idx, rps) = dpb.dpb_picture_start_with_rps(
                self.frame_count,        // frame_id
                self.poc_counter as u32, // pic_order_cnt_val
                pic_type,
                is_irap,
                is_idr,
                true, // pic_output_flag
                0,    // temporal_id
                no_output_of_prior_pics,
                self.frame_count, // time_stamp
                &strps_for_start,
                max_pic_order_cnt_lsb,
            );

            tracing::debug!(slot_idx = slot_idx, "  Step 3 result: allocated DPB slot");

            // Step 4: SetupReferencePictureListLx (build RefPicList0/1)
            tracing::debug!(
                pic_type = pic_type,
                "H265 encode Step 4: setup_reference_picture_list_lx"
            );
            let ref_list_res = if pic_type == 2 || pic_type == 3 {
                // P or B frame
                Some(dpb.setup_reference_picture_list_lx(pic_type, &rps, 1, 0))
            } else {
                None
            };

            // Build reference slot list from RefPicList0
            h265_ref_slot_list = if let Some(ref rl) = ref_list_res {
                let mut list = Vec::new();
                for i in 0..=rl.num_ref_idx_l0_active_minus1 as usize {
                    if rl.ref_pic_list0[i] != NO_REFERENCE_PICTURE_H265 {
                        list.push(rl.ref_pic_list0[i]);
                    }
                }
                if list.is_empty() && (pic_type == 2 || pic_type == 3) {
                    tracing::warn!(
                        pic_type = pic_type,
                        num_ref_idx_l0_active_minus1 = rl.num_ref_idx_l0_active_minus1,
                        "EMPTY reference slot list for P/B frame after DPB pipeline"
                    );
                }
                tracing::debug!(?list, "  reference_slot_list from RefPicList0");
                list
            } else {
                tracing::debug!("  No ref_list_res (IDR/I frame)");
                Vec::new()
            };

            // Step 5: DpbPictureEnd
            tracing::debug!(
                is_reference = is_reference,
                "H265 encode Step 5: dpb_picture_end"
            );
            dpb.dpb_picture_end(1, is_reference);

            h265_setup_slot = if slot_idx >= 0 {
                slot_idx
            } else {
                (self.frame_count as usize % self.dpb_slots.len()) as i8
            };

            h265_rps_result = rps_res;
            _h265_ref_pic_set = rps;
            h265_ref_list_result = ref_list_res;

            // H.264 variables unused for H.265
            h264_frame_num = 0;
            h264_poc = 0;
            h264_setup_slot = -1;
            h264_ref_slot_list = Vec::new();
            h264_ref_slot_list_l1 = Vec::new();
        } else if is_h264 && let Some(enc) = self.h264_encoder.as_mut() {
            // H.264 DPB-managed path via VkVideoEncoderH264: handles FrameNum, POC,
            // reference list construction, sliding window, and DPB slot management.
            if is_idr {
                enc.frame_num = 0;
                enc.poc_lsb = 0;
                enc.idr_pic_id = enc.idr_pic_id.wrapping_add(1);
            }

            let pic_info = PicInfoH264 {
                frame_num: enc.frame_num,
                pic_order_cnt: enc.poc_lsb,
                is_idr,
                is_reference,
                primary_pic_type: match frame_type {
                    FrameType::Idr | FrameType::I => 0,
                    FrameType::P => 1,
                    FrameType::B => 2,
                },
                idr_pic_id: enc.idr_pic_id,
                ..Default::default()
            };

            let dpb = enc
                .dpb264
                .as_mut()
                .expect("an H.264 encoder gets its DPB from init_dpb when its session is created");
            dpb.dpb_picture_start(
                &pic_info,
                enc.log2_max_frame_num_minus4,
                0, // POC type 0
                enc.log2_max_pic_order_cnt_lsb_minus4,
                false,
                enc.max_num_ref_frames,
            );

            let (fn_val, poc_val) = dpb.get_updated_frame_num_and_pic_order_cnt();
            h264_frame_num = fn_val;
            h264_poc = poc_val;

            // Build reference list from DPB state BEFORE dpb_picture_end
            // modifies it (sorted by descending PicNum for P-frames).
            let ref_lists = if frame_type == FrameType::P {
                dpb.build_ref_pic_list_p()
            } else if frame_type == FrameType::B {
                dpb.build_ref_pic_list_b(poc_val)
            } else {
                Default::default()
            };
            let ref_count_l0 = ref_lists.ref_pic_list_count[0] as usize;
            h264_ref_slot_list = ref_lists.ref_pic_list[0][..ref_count_l0].to_vec();
            let ref_count_l1 = ref_lists.ref_pic_list_count[1] as usize;
            h264_ref_slot_list_l1 = ref_lists.ref_pic_list[1][..ref_count_l1].to_vec();

            // dpb_picture_end: applies sliding window, stores picture in DPB.
            // Returns the DPB slot index for the setup (reconstructed) picture.
            let slot = dpb.dpb_picture_end(&pic_info, enc.max_num_ref_frames);
            h264_setup_slot = if slot >= 0 {
                slot
            } else {
                (self.frame_count as usize % self.dpb_slots.len()) as i8
            };

            // H.265 variables unused for H.264
            h265_setup_slot = -1;
            h265_ref_slot_list = Vec::new();
            h265_poc = 0;
            h265_rps_result = None;
            _h265_ref_pic_set = RefPicSetH265::default();
            h265_ref_list_result = None;
        } else {
            h264_frame_num = self.frame_count as u32;
            h264_poc = (self.poc_counter * 2) as i32;
            h264_setup_slot = -1;
            h264_ref_slot_list = Vec::new();
            h264_ref_slot_list_l1 = Vec::new();
            h265_setup_slot = -1;
            h265_ref_slot_list = Vec::new();
            h265_poc = self.poc_counter as i32;
            h265_rps_result = None;
            _h265_ref_pic_set = RefPicSetH265::default();
            h265_ref_list_result = None;
        }

        // --- Select DPB slots ---
        let setup_slot_index = if is_h264 && h264_setup_slot >= 0 {
            h264_setup_slot as usize
        } else if is_h265 && h265_setup_slot >= 0 {
            h265_setup_slot as usize
        } else {
            (self.frame_count as usize) % self.dpb_slots.len()
        };

        // Reference slots: DPB-sorted list for H.264/H.265, legacy for others.
        // Exclude the setup slot to prevent read/write conflicts (the
        // sliding window may reuse a slot that was in the reference list).
        // For B-frames, include both L0 (backward) and L1 (forward) refs.
        let mut reference_slot_indices: Vec<usize> = Vec::new();
        if is_h265 && !h265_ref_slot_list.is_empty() {
            // H.265 DPB-managed path: use sorted reference list
            let mut seen = [false; 16];
            for &slot_idx in &h265_ref_slot_list {
                let idx = slot_idx as usize;
                if idx != setup_slot_index && idx < 16 && !seen[idx] {
                    seen[idx] = true;
                    reference_slot_indices.push(idx);
                }
            }
            tracing::debug!(
                setup_slot_index = setup_slot_index,
                ?reference_slot_indices,
                "H265 reference_slot_indices (after filtering setup slot)"
            );
        } else if is_h264 && (!h264_ref_slot_list.is_empty() || !h264_ref_slot_list_l1.is_empty()) {
            // Collect unique slot indices from L0 + L1, excluding setup slot
            let mut seen = [false; 16];
            for &slot_idx in h264_ref_slot_list
                .iter()
                .chain(h264_ref_slot_list_l1.iter())
            {
                let idx = slot_idx as usize;
                if idx != setup_slot_index && idx < 16 && !seen[idx] {
                    seen[idx] = true;
                    reference_slot_indices.push(idx);
                }
            }
        } else if (frame_type == FrameType::P || frame_type == FrameType::B) && self.frame_count > 0
        {
            let prev_slot = ((self.frame_count - 1) as usize) % self.dpb_slots.len();
            if self.dpb_slots[prev_slot].in_use {
                reference_slot_indices.push(prev_slot);
            }
        }

        EncodeFrameReferencePlan {
            h264_frame_num,
            h264_poc,
            h264_ref_slot_list,
            h264_ref_slot_list_l1,
            h265_poc,
            h265_rps_result,
            h265_ref_list_result,
            is_h264,
            is_h265,
            is_reference,
            setup_slot_index,
            reference_slot_indices,
        }
    }
}

/// A frame the GPU has coded: what its DPB slot and the codec counters
/// record of it.
pub(super) struct CodedFrame {
    pub(super) frame_type: FrameType,
    pub(super) setup_slot_index: usize,
    pub(super) is_reference: bool,
    pub(super) is_h264: bool,
    pub(super) is_h265: bool,
    pub(super) h264_frame_num: u32,
    pub(super) h264_poc: i32,
    pub(super) h265_poc: i32,
}

impl SimpleEncoder {
    /// Record `coded` in its DPB slot and advance the frame counters, once
    /// its coding has completed.
    pub(super) fn record_the_coded_frame(&mut self, coded: CodedFrame) {
        let CodedFrame {
            frame_type,
            setup_slot_index,
            is_reference,
            is_h264,
            is_h265,
            h264_frame_num,
            h264_poc,
            h265_poc,
        } = coded;
        let h265_pic_type = match frame_type {
            FrameType::Idr => vk::video::STD_VIDEO_H265_PICTURE_TYPE_IDR,
            FrameType::I => vk::video::STD_VIDEO_H265_PICTURE_TYPE_I,
            FrameType::P => vk::video::STD_VIDEO_H265_PICTURE_TYPE_P,
            FrameType::B => vk::video::STD_VIDEO_H265_PICTURE_TYPE_B,
        };
        // --- Update DPB slot ---
        // Only reference frames (IDR/I/P) persist in the DPB.
        // B-frames are non-reference: the slot can be reused immediately.
        self.dpb_slots[setup_slot_index].in_use = is_reference;
        // Record the picture type so references carry the correct type
        // (e.g. IDR references are marked as IDR, not P).
        self.dpb_slots[setup_slot_index].pic_type = match frame_type {
            FrameType::Idr => vk::video::STD_VIDEO_H264_PICTURE_TYPE_IDR,
            FrameType::I => vk::video::STD_VIDEO_H264_PICTURE_TYPE_I,
            FrameType::P => vk::video::STD_VIDEO_H264_PICTURE_TYPE_P,
            FrameType::B => vk::video::STD_VIDEO_H264_PICTURE_TYPE_B,
        };
        self.dpb_slots[setup_slot_index].h265_pic_type = h265_pic_type;
        if is_h265 && self.h265_encoder.is_some() {
            // H.265: use DPB-managed POC
            self.dpb_slots[setup_slot_index].frame_num = self.frame_count;
            self.dpb_slots[setup_slot_index].poc = h265_poc;
        } else if is_h264 && let Some(enc) = self.h264_encoder.as_mut() {
            // H.264: use DPB-provided frame_num and POC
            self.dpb_slots[setup_slot_index].frame_num = h264_frame_num as u64;
            self.dpb_slots[setup_slot_index].poc = h264_poc;

            // Advance H.264-specific counters via VkVideoEncoderH264
            if is_reference {
                let max_frame_num = 1u32 << (enc.log2_max_frame_num_minus4 + 4);
                enc.frame_num = (enc.frame_num + 1) % max_frame_num;
            }
            let max_poc_lsb = 1i32 << (enc.log2_max_pic_order_cnt_lsb_minus4 + 4);
            enc.poc_lsb = (enc.poc_lsb + 2) % max_poc_lsb;
        } else {
            // Fallback: use poc_counter
            self.dpb_slots[setup_slot_index].frame_num = self.frame_count;
            self.dpb_slots[setup_slot_index].poc =
                if self.codec_flag == vk::VideoCodecOperationFlagsKHR::ENCODE_H265 {
                    self.poc_counter as i32
                } else {
                    (self.poc_counter * 2) as i32
                };
        }

        // --- Advance counters ---
        self.frame_count += 1;
        self.poc_counter += 1;
        self.encode_order_count += 1;
    }
}
