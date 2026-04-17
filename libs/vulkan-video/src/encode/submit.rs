// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Encode frame submission: DPB management, reference list construction,
//! rate control, codec-specific encode info assembly, and command recording.

use vulkanalia::prelude::v1_4::*;
use vulkanalia::vk;
use vulkanalia::vk::KhrVideoQueueExtensionDeviceCommands;
use vulkanalia::vk::KhrVideoEncodeQueueExtensionDeviceCommands;
use std::ptr;

use crate::video_context::{VideoError, VideoResult};
use crate::vk_video_encoder::vk_encoder_dpb_h264::PicInfoH264;
use crate::vk_video_encoder::vk_encoder_dpb_h265::{
    RefPicSetH265, SetupRefListResult,
    NO_REFERENCE_PICTURE_H265, MAX_NUM_LIST_REF_H265,
};

use super::config::{EncodedOutput, EncodeFeedback, FrameType, RateControlMode};
use super::SimpleEncoder;

impl SimpleEncoder {
    /// Encode a single frame.
    ///
    /// The caller provides their own `VkImage` + `VkImageView` which must be
    /// in `VK_IMAGE_LAYOUT_VIDEO_ENCODE_SRC_KHR` layout. The image dimensions
    /// must match the configured encode size.
    ///
    /// Returns the encoded bitstream data for this frame.
    ///
    /// # Safety
    ///
    /// The caller must ensure `source_image` and `source_view` are valid and
    /// that the image is in `VIDEO_ENCODE_SRC_KHR` layout.
    pub(crate) unsafe fn encode_frame(
        &mut self,
        _source_image: vk::Image,
        source_view: vk::ImageView,
        frame_type: FrameType,
    ) -> VideoResult<EncodedOutput> {
        if !self.configured {
            return Err(VideoError::BitstreamError(
                "Encoder not configured -- call configure() first".to_string(),
            ));
        }

        let config = self.encode_config.as_ref().unwrap();
        let aligned_w = self.aligned_width;
        let aligned_h = self.aligned_height;
        let pts = self.frame_count;
        let encode_order = self.encode_order_count;
        let is_idr = frame_type == FrameType::Idr;
        // Reset POC counter on IDR frames. H.265 requires POC to restart
        // from 0 at each IDR; without this, P-frames after the second IDR
        // reference POC values that were never emitted in the bitstream.
        if is_idr {
            self.poc_counter = 0;
            // Invalidate all DPB slots on IDR -- no references should persist
            // across IDR boundaries.
            for slot in &mut self.dpb_slots {
                slot.in_use = false;
            }
        }
        let device = self.ctx.device();
        tracing::debug!(
            pts = pts,
            frame_type = frame_type.name(),
            "Encoding frame"
        );

        // --- DPB-managed frame info ---
        // When the codec DPB is active, use it for POC, reference lists, and
        // DPB slot management instead of the inline logic.
        let h264_frame_num: u32;
        let h264_poc: i32;
        let h264_setup_slot: i8;
        let h264_ref_slot_list: Vec<u8>;   // L0 (backward references)
        let h264_ref_slot_list_l1: Vec<u8>; // L1 (forward references, B-frames only)

        // H.265 DPB-managed variables
        let h265_setup_slot: i8;
        let h265_ref_slot_list: Vec<u8>;   // L0 (backward references)
        let h265_poc: i32;
        // Results from the H.265 DPB pipeline (initialized to defaults for non-H.265)
        let h265_rps_result: Option<crate::vk_video_encoder::vk_encoder_dpb_h265::InitializeRpsResult>;
        let _h265_ref_pic_set: RefPicSetH265;
        let h265_ref_list_result: Option<SetupRefListResult>;

        let is_h264 = self.codec_flag == vk::VideoCodecOperationFlagsKHR::ENCODE_H264;
        let is_h265 = self.codec_flag == vk::VideoCodecOperationFlagsKHR::ENCODE_H265;
        let is_reference = frame_type != FrameType::B;

        if is_h265 && self.h265_encoder.is_some() {
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

            let enc = self.h265_encoder.as_mut().unwrap();
            let h265_log2_max_poc_lsb_m4 = enc.log2_max_pic_order_cnt_lsb_minus4;
            let dpb = &mut enc.dpb;

            // Step 1: Reference picture marking (sliding window, CRA handling)
            tracing::debug!(h265_poc = h265_poc, pic_type = pic_type, "H265 encode Step 1: reference_picture_marking");
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
                self.frame_count,         // frame_id
                self.poc_counter as u32,  // pic_order_cnt_val
                pic_type,
                is_irap,
                is_idr,
                true,                     // pic_output_flag
                0,                        // temporal_id
                no_output_of_prior_pics,
                self.frame_count,         // time_stamp
                &strps_for_start,
                max_pic_order_cnt_lsb,
            );

            tracing::debug!(
                slot_idx = slot_idx,
                "  Step 3 result: allocated DPB slot"
            );

            // Step 4: SetupReferencePictureListLx (build RefPicList0/1)
            tracing::debug!(pic_type = pic_type, "H265 encode Step 4: setup_reference_picture_list_lx");
            let ref_list_res = if pic_type == 2 || pic_type == 3 {
                // P or B frame
                Some(dpb.setup_reference_picture_list_lx(
                    pic_type, &rps, 1, 0,
                ))
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
            tracing::debug!(is_reference = is_reference, "H265 encode Step 5: dpb_picture_end");
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
        } else if is_h264 && self.h264_encoder.is_some() {
            // H.264 DPB-managed path via VkVideoEncoderH264: handles FrameNum, POC,
            // reference list construction, sliding window, and DPB slot management.
            let enc = self.h264_encoder.as_mut().unwrap();
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

            let dpb = enc.dpb264.as_mut().unwrap();
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
            for &slot_idx in h264_ref_slot_list.iter().chain(h264_ref_slot_list_l1.iter()) {
                let idx = slot_idx as usize;
                if idx != setup_slot_index && idx < 16 && !seen[idx] {
                    seen[idx] = true;
                    reference_slot_indices.push(idx);
                }
            }
        } else if frame_type == FrameType::P || frame_type == FrameType::B {
            if self.frame_count > 0 {
                let prev_slot = ((self.frame_count - 1) as usize) % self.dpb_slots.len();
                if self.dpb_slots[prev_slot].in_use {
                    reference_slot_indices.push(prev_slot);
                }
            }
        }

        // --- Reset command buffer and begin recording ---
        device.reset_command_buffer(
            self.command_buffer,
            vk::CommandBufferResetFlags::empty(),
        )?;

        let begin_info = vk::CommandBufferBeginInfo::builder()
            .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT);

        device.begin_command_buffer(self.command_buffer, &begin_info)?;

        // --- Reset query ---
        device.cmd_reset_query_pool(self.command_buffer, self.query_pool, 0, 1);

        // --- Build reference slot info structures ---
        let mut dpb_resource_infos: Vec<vk::VideoPictureResourceInfoKHR> = Vec::new();
        let mut ref_slot_infos: Vec<vk::VideoReferenceSlotInfoKHR> = Vec::new();

        for &ref_idx in &reference_slot_indices {
            let slot = &self.dpb_slots[ref_idx];
            dpb_resource_infos.push(
                *vk::VideoPictureResourceInfoKHR::builder()
                    .coded_offset(vk::Offset2D { x: 0, y: 0 })
                    .coded_extent(vk::Extent2D {
                        width: aligned_w,
                        height: aligned_h,
                    })
                    .base_array_layer(0)
                    .image_view_binding(slot.view),
            );
        }

        for (i, &ref_idx) in reference_slot_indices.iter().enumerate() {
            ref_slot_infos.push(
                *vk::VideoReferenceSlotInfoKHR::builder()
                    .slot_index(ref_idx as i32)
                    .picture_resource(&dpb_resource_infos[i]),
            );
        }

        // Setup slot resource (where the encoded picture will be stored in DPB)
        let setup_resource = vk::VideoPictureResourceInfoKHR::builder()
            .coded_offset(vk::Offset2D { x: 0, y: 0 })
            .coded_extent(vk::Extent2D {
                width: aligned_w,
                height: aligned_h,
            })
            .base_array_layer(0)
            .image_view_binding(self.dpb_slots[setup_slot_index].view);

        // --- Build codec-specific DPB slot info for setup and reference slots ---
        // The Vulkan Video spec requires each reference slot's pNext chain to
        // include the codec-specific DPB slot info structure.

        // Setup slot reference info (H.264)
        let h264_setup_ref_info_flags: vk::video::StdVideoEncodeH264ReferenceInfoFlags =
            std::mem::zeroed();
        let h264_setup_ref_info = vk::video::StdVideoEncodeH264ReferenceInfo {
            flags: h264_setup_ref_info_flags,
            primary_pic_type: match frame_type {
                FrameType::Idr | FrameType::I => {
                    vk::video::STD_VIDEO_H264_PICTURE_TYPE_IDR
                }
                FrameType::P => {
                    vk::video::STD_VIDEO_H264_PICTURE_TYPE_P
                }
                FrameType::B => {
                    vk::video::STD_VIDEO_H264_PICTURE_TYPE_B
                }
            },
            FrameNum: h264_frame_num,
            PicOrderCnt: h264_poc,
            long_term_pic_num: 0,
            long_term_frame_idx: 0,
            temporal_id: 0,
        };
        let mut h264_setup_dpb_slot = vk::VideoEncodeH264DpbSlotInfoKHR::builder()
            .std_reference_info(&h264_setup_ref_info);

        // Setup slot reference info (H.265)
        // Populate with correct pic_type and POC for the current frame.
        let h265_setup_pic_type = match frame_type {
            FrameType::Idr => vk::video::STD_VIDEO_H265_PICTURE_TYPE_IDR,
            FrameType::I => vk::video::STD_VIDEO_H265_PICTURE_TYPE_I,
            FrameType::P => vk::video::STD_VIDEO_H265_PICTURE_TYPE_P,
            FrameType::B => vk::video::STD_VIDEO_H265_PICTURE_TYPE_B,
        };
        let h265_setup_ref_info_flags: vk::video::StdVideoEncodeH265ReferenceInfoFlags =
            std::mem::zeroed();
        let h265_setup_ref_info = vk::video::StdVideoEncodeH265ReferenceInfo {
            flags: h265_setup_ref_info_flags,
            pic_type: h265_setup_pic_type,
            PicOrderCntVal: h265_poc,
            TemporalId: 0,
        };
        let mut h265_setup_dpb_slot = vk::VideoEncodeH265DpbSlotInfoKHR::builder()
            .std_reference_info(&h265_setup_ref_info);

        if is_h265 {
            tracing::debug!(
                setup_slot_index = setup_slot_index,
                h265_poc = h265_poc,
                pic_type = ?h265_setup_pic_type,
                "H265 setup VkVideoReferenceSlotInfoKHR"
            );
        }

        let mut setup_slot = vk::VideoReferenceSlotInfoKHR::builder()
            .slot_index(setup_slot_index as i32)
            .picture_resource(&setup_resource);

        if self.codec_flag == vk::VideoCodecOperationFlagsKHR::ENCODE_H264 {
            setup_slot = setup_slot.push_next(&mut h264_setup_dpb_slot);
        } else if self.codec_flag == vk::VideoCodecOperationFlagsKHR::ENCODE_H265 {
            setup_slot = setup_slot.push_next(&mut h265_setup_dpb_slot);
        }

        // Build codec-specific DPB slot info for reference slots
        let mut h264_ref_dpb_infos: Vec<vk::video::StdVideoEncodeH264ReferenceInfo> =
            Vec::new();
        let mut h264_ref_dpb_slot_infos: Vec<vk::VideoEncodeH264DpbSlotInfoKHR> = Vec::new();
        let mut h265_ref_dpb_infos: Vec<vk::video::StdVideoEncodeH265ReferenceInfo> =
            Vec::new();
        let mut h265_ref_dpb_slot_infos: Vec<vk::VideoEncodeH265DpbSlotInfoKHR> = Vec::new();

        for &ref_idx in &reference_slot_indices {
            let slot = &self.dpb_slots[ref_idx];
            if self.codec_flag == vk::VideoCodecOperationFlagsKHR::ENCODE_H264 {
                // Use DPB-provided reference info when available
                let (ref_poc, ref_is_long_term, ref_long_term_idx) =
                    if let Some(ref enc) = self.h264_encoder {
                        if let Some(ref dpb) = enc.dpb264 {
                            let mut poc = 0i32;
                            let mut is_lt = false;
                            let mut lt_idx = -1i32;
                            dpb.fill_std_reference_info(ref_idx as u8, &mut poc, &mut is_lt, &mut lt_idx);
                            (poc, is_lt, lt_idx)
                        } else {
                            (slot.poc, false, -1i32)
                        }
                    } else {
                        (slot.poc, false, -1i32)
                    };
                let ref_flags: vk::video::StdVideoEncodeH264ReferenceInfoFlags =
                    std::mem::zeroed();
                h264_ref_dpb_infos.push(vk::video::StdVideoEncodeH264ReferenceInfo {
                    flags: ref_flags,
                    primary_pic_type: slot.pic_type,
                    FrameNum: slot.frame_num as u32,
                    PicOrderCnt: ref_poc,
                    long_term_pic_num: 0,
                    long_term_frame_idx: if ref_is_long_term { ref_long_term_idx as u16 } else { 0 },
                    temporal_id: 0,
                });
            } else if self.codec_flag == vk::VideoCodecOperationFlagsKHR::ENCODE_H265 {
                // Use DPB-provided reference info when available
                let (ref_poc, ref_tid) =
                    if let Some(ref enc) = self.h265_encoder {
                        let mut poc = 0u32;
                        let mut tid = 0i32;
                        let mut _unused = false;
                        enc.dpb.fill_std_reference_info(ref_idx as u8, &mut poc, &mut tid, &mut _unused);
                        (poc as i32, tid)
                    } else {
                        (slot.poc, 0)
                    };
                tracing::debug!(
                    ref_slot = ref_idx,
                    ref_poc = ref_poc,
                    ref_tid = ref_tid,
                    pic_type = ?slot.h265_pic_type,
                    "H265 reference DPB slot info"
                );
                let ref_flags: vk::video::StdVideoEncodeH265ReferenceInfoFlags =
                    std::mem::zeroed();
                h265_ref_dpb_infos.push(vk::video::StdVideoEncodeH265ReferenceInfo {
                    flags: ref_flags,
                    pic_type: slot.h265_pic_type,
                    PicOrderCntVal: ref_poc,
                    TemporalId: ref_tid as u8,
                });
            }
        }

        // Create DPB slot info wrappers (must be done after infos are stable in memory)
        if self.codec_flag == vk::VideoCodecOperationFlagsKHR::ENCODE_H264 {
            for info in &h264_ref_dpb_infos {
                h264_ref_dpb_slot_infos.push(
                    *vk::VideoEncodeH264DpbSlotInfoKHR::builder().std_reference_info(info),
                );
            }
        } else if self.codec_flag == vk::VideoCodecOperationFlagsKHR::ENCODE_H265 {
            for info in &h265_ref_dpb_infos {
                h265_ref_dpb_slot_infos.push(
                    *vk::VideoEncodeH265DpbSlotInfoKHR::builder().std_reference_info(info),
                );
            }
        }

        // Attach codec-specific pNext to reference slot infos via raw pointer
        if self.codec_flag == vk::VideoCodecOperationFlagsKHR::ENCODE_H264 {
            for (i, slot_info) in h264_ref_dpb_slot_infos.iter_mut().enumerate() {
                ref_slot_infos[i].next = slot_info as *mut _ as *const std::ffi::c_void;
            }
        } else if self.codec_flag == vk::VideoCodecOperationFlagsKHR::ENCODE_H265 {
            for (i, slot_info) in h265_ref_dpb_slot_infos.iter_mut().enumerate() {
                ref_slot_infos[i].next = slot_info as *mut _ as *const std::ffi::c_void;
            }
        }

        // All slots for BeginVideoCoding: must include all DPB picture resources
        // that will be used during the coding scope. The setup slot must always
        // be listed so its picture resource is "bound". For slots being activated
        // for the first time, use slot_index = -1 per the Vulkan spec.
        let mut all_slots: Vec<vk::VideoReferenceSlotInfoKHR> = Vec::new();

        // C++ reference ALWAYS uses -1 for the setup slot in BeginVideoCoding.
        // Using the real index for previously-used slots causes the driver to
        // associate the OLD picture with the slot, corrupting P→P reconstruction.
        let begin_setup_slot_index: i32 = -1;

        // We need a separate setup slot for BeginVideoCoding that may use -1.
        // The encode command's setup_reference_slot always uses the real index.
        let mut begin_setup_slot = *vk::VideoReferenceSlotInfoKHR::builder()
            .slot_index(begin_setup_slot_index)
            .picture_resource(&setup_resource);

        // Attach codec-specific pNext to the begin setup slot too
        let mut h264_begin_setup_dpb = *vk::VideoEncodeH264DpbSlotInfoKHR::builder()
            .std_reference_info(&h264_setup_ref_info);
        let mut h265_begin_setup_dpb = *vk::VideoEncodeH265DpbSlotInfoKHR::builder()
            .std_reference_info(&h265_setup_ref_info);

        if self.codec_flag == vk::VideoCodecOperationFlagsKHR::ENCODE_H264 {
            begin_setup_slot.next = &mut h264_begin_setup_dpb as *mut _ as *const std::ffi::c_void;
        } else if self.codec_flag == vk::VideoCodecOperationFlagsKHR::ENCODE_H265 {
            begin_setup_slot.next = &mut h265_begin_setup_dpb as *mut _ as *const std::ffi::c_void;
        }

        all_slots.push(begin_setup_slot);
        all_slots.extend_from_slice(&ref_slot_infos);

        if is_h265 {
            tracing::debug!(
                total_slots = all_slots.len(),
                begin_setup_slot_index = begin_setup_slot_index,
                ref_slot_count = ref_slot_infos.len(),
                "H265 BeginVideoCoding all_slots"
            );
            for (i, slot) in all_slots.iter().enumerate() {
                tracing::debug!(
                    idx = i,
                    slot_index = slot.slot_index,
                    "  all_slots entry"
                );
            }
        }

        // --- Rate control info for BeginVideoCoding ---
        // Per Vulkan spec: if the rate control mode is not DEFAULT, the
        // VkVideoEncodeRateControlInfoKHR must be included in the pNext chain
        // of VkVideoBeginCodingInfoKHR for every coding scope.
        let rc_mode = config.rate_control_mode.to_vk_flags();

        let rate_control_layer = vk::VideoEncodeRateControlLayerInfoKHR::builder()
            .average_bitrate(config.average_bitrate as u64)
            .max_bitrate(config.max_bitrate as u64)
            .frame_rate_numerator(config.framerate_numerator)
            .frame_rate_denominator(config.framerate_denominator);

        // Per Vulkan spec: when rate_control_mode is DEFAULT or DISABLED (CQP),
        // layerCount must be 0. Only include layers for CBR/VBR modes.
        let needs_layers = rc_mode != vk::VideoEncodeRateControlModeFlagsKHR::DEFAULT
            && rc_mode != vk::VideoEncodeRateControlModeFlagsKHR::DISABLED;

        let mut begin_rc_info = vk::VideoEncodeRateControlInfoKHR::builder()
            .rate_control_mode(rc_mode);

        if needs_layers {
            begin_rc_info = begin_rc_info
                .layers(std::slice::from_ref(&rate_control_layer));
        }

        // --- vkCmdBeginVideoCodingKHR ---
        let mut begin_coding_info = vk::VideoBeginCodingInfoKHR::builder()
            .video_session(self.video_session)
            .video_session_parameters(self.session_params)
            .reference_slots(&all_slots);

        // Per Vulkan spec: the rate control info in BeginVideoCoding must match
        // the currently configured device state. On the first frame the session
        // is in DEFAULT mode (we change it via ControlVideoCoding below), so we
        // must NOT include rate control in Begin. On subsequent frames, include
        // it to match the configured state.
        if self.rate_control_sent && config.rate_control_mode != RateControlMode::Default {
            begin_coding_info = begin_coding_info.push_next(&mut begin_rc_info);
        }

        device.cmd_begin_video_coding_khr(self.command_buffer, &begin_coding_info);

        // --- vkCmdControlVideoCodingKHR (reset + rate control, on first frame) ---
        if !self.rate_control_sent {
            let mut control_rc_info = vk::VideoEncodeRateControlInfoKHR::builder()
                .rate_control_mode(rc_mode);

            if needs_layers {
                control_rc_info = control_rc_info
                    .layers(std::slice::from_ref(&rate_control_layer));
            }

            let mut control_flags = vk::VideoCodingControlFlagsKHR::RESET;
            if config.rate_control_mode != RateControlMode::Default {
                control_flags |= vk::VideoCodingControlFlagsKHR::ENCODE_RATE_CONTROL;
            }
            if self.effective_quality_level > 0 {
                control_flags |= vk::VideoCodingControlFlagsKHR::ENCODE_QUALITY_LEVEL;
            }

            let mut quality_level_info;
            let mut control_info = vk::VideoCodingControlInfoKHR::builder()
                .flags(control_flags)
                .push_next(&mut control_rc_info);

            if self.effective_quality_level > 0 {
                quality_level_info = vk::VideoEncodeQualityLevelInfoKHR::builder()
                    .quality_level(self.effective_quality_level);
                control_info = control_info.push_next(&mut quality_level_info);
            }

            device.cmd_control_video_coding_khr(self.command_buffer, &control_info);

            self.rate_control_sent = true;
        }

        // --- Source picture resource (caller's image) ---
        let src_resource = vk::VideoPictureResourceInfoKHR::builder()
            .coded_offset(vk::Offset2D { x: 0, y: 0 })
            .coded_extent(vk::Extent2D {
                width: aligned_w,
                height: aligned_h,
            })
            .base_array_layer(0)
            .image_view_binding(source_view);

        // --- Build codec-specific encode info ---
        // All StdVideo* structs must outlive the encode command, so declare them
        // here and conditionally populate below.
        let h264_slice_header;
        let h264_ref_lists;
        let h264_nalu_slice;
        let h264_std_pic_info;
        let mut h264_pic_info;

        let h265_slice_header;
        let h265_ref_lists;
        let h265_nalu_slice;
        let h265_std_pic_info;
        let mut h265_pic_info;
        let h265_short_term_ref_pic_set;

        let mut encode_info = vk::VideoEncodeInfoKHR::builder()
            .dst_buffer(self.bitstream_buffer)
            .dst_buffer_offset(0)
            .dst_buffer_range(self.bitstream_buffer_size as u64)
            .src_picture_resource(src_resource)
            .setup_reference_slot(&setup_slot)
            .reference_slots(&ref_slot_infos);

        if self.codec_flag == vk::VideoCodecOperationFlagsKHR::ENCODE_H264 {
            let primary_pic_type = match frame_type {
                FrameType::Idr | FrameType::I => {
                    vk::video::STD_VIDEO_H264_PICTURE_TYPE_IDR
                }
                FrameType::P => {
                    vk::video::STD_VIDEO_H264_PICTURE_TYPE_P
                }
                FrameType::B => {
                    vk::video::STD_VIDEO_H264_PICTURE_TYPE_B
                }
            };

            // H.264 slice type matching the frame type
            let h264_slice_type = match frame_type {
                FrameType::Idr | FrameType::I => {
                    vk::video::STD_VIDEO_H264_SLICE_TYPE_I
                }
                FrameType::P => {
                    vk::video::STD_VIDEO_H264_SLICE_TYPE_P
                }
                FrameType::B => {
                    vk::video::STD_VIDEO_H264_SLICE_TYPE_B
                }
            };

            // Build StdVideoEncodeH264SliceHeader (required by spec, was NULL causing segfault)
            let mut slice_hdr_flags: vk::video::StdVideoEncodeH264SliceHeaderFlags =
                std::mem::zeroed();
            // Signal the reference index override in the slice header so the
            // decoder knows how many references are active. Without this, the
            // decoder falls back to the PPS default (1 ref) which causes a
            // CABAC context mismatch when the encoder uses multiple references.
            if frame_type == FrameType::P || frame_type == FrameType::B {
                slice_hdr_flags.set_num_ref_idx_active_override_flag(1);
            }
            h264_slice_header = vk::video::StdVideoEncodeH264SliceHeader {
                flags: slice_hdr_flags,
                first_mb_in_slice: 0,
                slice_type: h264_slice_type,
                slice_alpha_c0_offset_div2: 0,
                slice_beta_offset_div2: 0,
                slice_qp_delta: 0,
                reserved1: 0,
                cabac_init_idc:
                    vk::video::STD_VIDEO_H264_CABAC_INIT_IDC_0,
                disable_deblocking_filter_idc:
                    vk::video::STD_VIDEO_H264_DISABLE_DEBLOCKING_FILTER_IDC_DISABLED,
                pWeightTable: ptr::null(),
            };

            // Build reference lists for P/B frames
            let mut std_flags: vk::video::StdVideoEncodeH264PictureInfoFlags =
                std::mem::zeroed();
            std_flags.set_IdrPicFlag(if is_idr { 1 } else { 0 });
            std_flags.set_is_reference(if frame_type != FrameType::B { 1 } else { 0 });

            let ref_lists_ptr = if !reference_slot_indices.is_empty() {
                // Build RefPicList0 (backward refs) and RefPicList1 (forward refs).
                // For P-frames only L0 is used; for B-frames both L0 and L1.
                // Use the DPB-sorted lists when available, fall back to
                // reference_slot_indices for the legacy path.
                let mut ref_pic_list0 = [0xffu8; 32];
                let mut ref_pic_list1 = [0xffu8; 32];

                // RefPicList0/1 entries must only reference slots that are
                // present in reference_slot_indices (i.e. have a matching
                // VkVideoReferenceSlotInfoKHR). The setup slot was excluded
                // from reference_slot_indices to prevent read/write conflicts,
                // so it must also be excluded from the ref pic lists.
                if !h264_ref_slot_list.is_empty() {
                    let mut j = 0;
                    for &slot_idx in &h264_ref_slot_list {
                        if (slot_idx as usize) != setup_slot_index && j < 32 {
                            ref_pic_list0[j] = slot_idx;
                            j += 1;
                        }
                    }
                } else {
                    for (i, &ref_idx) in reference_slot_indices.iter().enumerate() {
                        if i < 32 { ref_pic_list0[i] = ref_idx as u8; }
                    }
                }

                let l1_count = if frame_type == FrameType::B {
                    let mut j = 0;
                    for &slot_idx in &h264_ref_slot_list_l1 {
                        if (slot_idx as usize) != setup_slot_index && j < 32 {
                            ref_pic_list1[j] = slot_idx;
                            j += 1;
                        }
                    }
                    j
                } else {
                    0
                };

                // L0 count must match the filtered list, not the raw DPB list
                let l0_count = reference_slot_indices.len();

                let ref_lists_flags: vk::video::StdVideoEncodeH264ReferenceListsInfoFlags =
                    std::mem::zeroed();
                h264_ref_lists = vk::video::StdVideoEncodeH264ReferenceListsInfo {
                    flags: ref_lists_flags,
                    num_ref_idx_l0_active_minus1: (l0_count as u8).saturating_sub(1),
                    num_ref_idx_l1_active_minus1: if l1_count > 0 {
                        (l1_count as u8).saturating_sub(1)
                    } else {
                        0
                    },
                    RefPicList0: ref_pic_list0,
                    RefPicList1: ref_pic_list1,
                    refList0ModOpCount: 0,
                    refList1ModOpCount: 0,
                    refPicMarkingOpCount: 0,
                    reserved1: [0; 7],
                    pRefList0ModOperations: ptr::null(),
                    pRefList1ModOperations: ptr::null(),
                    pRefPicMarkingOperations: ptr::null(),
                };
                &h264_ref_lists as *const _
            } else {
                // IDR/I frames: no reference lists needed
                ptr::null()
            };

            h264_std_pic_info = vk::video::StdVideoEncodeH264PictureInfo {
                flags: std_flags,
                seq_parameter_set_id: 0,
                pic_parameter_set_id: 0,
                idr_pic_id: if is_idr {
                    self.h264_encoder.as_ref().map(|e| e.idr_pic_id).unwrap_or(0)
                } else { 0 },
                primary_pic_type,
                frame_num: h264_frame_num,
                PicOrderCnt: h264_poc,
                temporal_id: 0,
                reserved1: [0; 3],
                pRefLists: ref_lists_ptr,
            };

            let qp = if config.rate_control_mode == RateControlMode::Cqp {
                match frame_type {
                    FrameType::Idr | FrameType::I => config.const_qp_intra,
                    FrameType::P => config.const_qp_inter_p,
                    FrameType::B => config.const_qp_inter_b,
                }
            } else {
                0
            };

            h264_nalu_slice = vk::VideoEncodeH264NaluSliceInfoKHR::builder()
                .constant_qp(qp)
                .std_slice_header(&h264_slice_header);

            h264_pic_info = vk::VideoEncodeH264PictureInfoKHR::builder()
                .nalu_slice_entries(std::slice::from_ref(&h264_nalu_slice))
                .std_picture_info(&h264_std_pic_info);

            encode_info = encode_info.push_next(&mut h264_pic_info);
        } else if self.codec_flag == vk::VideoCodecOperationFlagsKHR::ENCODE_H265 {
            let pic_type = match frame_type {
                FrameType::Idr => {
                    vk::video::STD_VIDEO_H265_PICTURE_TYPE_IDR
                }
                FrameType::I => {
                    vk::video::STD_VIDEO_H265_PICTURE_TYPE_I
                }
                FrameType::P => {
                    vk::video::STD_VIDEO_H265_PICTURE_TYPE_P
                }
                FrameType::B => {
                    vk::video::STD_VIDEO_H265_PICTURE_TYPE_B
                }
            };

            // H.265 slice type
            let h265_slice_type = match frame_type {
                FrameType::Idr | FrameType::I => {
                    vk::video::STD_VIDEO_H265_SLICE_TYPE_I
                }
                FrameType::P => {
                    vk::video::STD_VIDEO_H265_SLICE_TYPE_P
                }
                FrameType::B => {
                    vk::video::STD_VIDEO_H265_SLICE_TYPE_B
                }
            };

            // Build StdVideoEncodeH265SliceSegmentHeader
            let mut slice_seg_hdr_flags: vk::video::StdVideoEncodeH265SliceSegmentHeaderFlags =
                std::mem::zeroed();
            slice_seg_hdr_flags.set_first_slice_segment_in_pic_flag(1);
            slice_seg_hdr_flags.set_slice_loop_filter_across_slices_enabled_flag(1);
            slice_seg_hdr_flags.set_slice_sao_luma_flag(1);
            slice_seg_hdr_flags.set_slice_sao_chroma_flag(1);
            slice_seg_hdr_flags.set_cu_chroma_qp_offset_enabled_flag(1);
            slice_seg_hdr_flags.set_deblocking_filter_override_flag(1);
            if frame_type == FrameType::P || frame_type == FrameType::B {
                slice_seg_hdr_flags.set_num_ref_idx_active_override_flag(1);
            }
            tracing::debug!(
                slice_type = ?h265_slice_type,
                first_slice_segment_in_pic = true,
                slice_sao_luma = true,
                slice_sao_chroma = true,
                num_ref_idx_active_override = (frame_type == FrameType::P || frame_type == FrameType::B),
                "H265 StdVideoEncodeH265SliceSegmentHeader"
            );
            h265_slice_header = vk::video::StdVideoEncodeH265SliceSegmentHeader {
                flags: slice_seg_hdr_flags,
                slice_type: h265_slice_type,
                slice_segment_address: 0,
                collocated_ref_idx: 0,
                MaxNumMergeCand: 5,
                slice_cb_qp_offset: 0,
                slice_cr_qp_offset: 0,
                slice_beta_offset_div2: 0,
                slice_tc_offset_div2: 0,
                slice_act_y_qp_offset: 0,
                slice_act_cb_qp_offset: 0,
                slice_act_cr_qp_offset: 0,
                slice_qp_delta: 0,
                reserved1: 0,
                pWeightTable: ptr::null(),
            };

            let mut std_flags: vk::video::StdVideoEncodeH265PictureInfoFlags =
                std::mem::zeroed();
            std_flags.set_is_reference(if frame_type != FrameType::B { 1 } else { 0 });
            std_flags.set_IrapPicFlag(if is_idr || frame_type == FrameType::I { 1 } else { 0 });
            std_flags.set_pic_output_flag(1);
            // Signal no_output_of_prior_pics on non-first IDR frames (C++ reference behavior).
            if is_idr && self.frame_count > 0 {
                std_flags.set_no_output_of_prior_pics_flag(1);
            }

            // Build reference lists from the DPB pipeline results.
            // setup_reference_picture_list_lx() already built RefPicList0/1.
            let ref_lists_ptr: *const vk::video::StdVideoEncodeH265ReferenceListsInfo = if let Some(ref rl) = h265_ref_list_result {
                let mut ref_pic_list0 = [NO_REFERENCE_PICTURE_H265; 15];
                let mut ref_pic_list1 = [NO_REFERENCE_PICTURE_H265; 15];
                let mut list_entry_l0 = [0u8; 15];
                let mut list_entry_l1 = [0u8; 15];

                for i in 0..MAX_NUM_LIST_REF_H265 {
                    ref_pic_list0[i] = rl.ref_pic_list0[i];
                    ref_pic_list1[i] = rl.ref_pic_list1[i];
                    list_entry_l0[i] = i as u8;
                    list_entry_l1[i] = i as u8;
                }

                tracing::debug!(
                    num_ref_idx_l0_active_minus1 = rl.num_ref_idx_l0_active_minus1,
                    num_ref_idx_l1_active_minus1 = rl.num_ref_idx_l1_active_minus1,
                    "H265 StdVideoEncodeH265ReferenceListsInfo"
                );
                for i in 0..=rl.num_ref_idx_l0_active_minus1 as usize {
                    tracing::debug!(
                        idx = i,
                        ref_pic_list0 = ref_pic_list0[i],
                        list_entry_l0 = list_entry_l0[i],
                        "  RefPicList0 entry in encode command"
                    );
                }

                let ref_lists_flags: vk::video::StdVideoEncodeH265ReferenceListsInfoFlags =
                    std::mem::zeroed();
                h265_ref_lists = vk::video::StdVideoEncodeH265ReferenceListsInfo {
                    flags: ref_lists_flags,
                    num_ref_idx_l0_active_minus1: rl.num_ref_idx_l0_active_minus1,
                    num_ref_idx_l1_active_minus1: rl.num_ref_idx_l1_active_minus1,
                    RefPicList0: ref_pic_list0,
                    RefPicList1: ref_pic_list1,
                    list_entry_l0,
                    list_entry_l1,
                };
                &h265_ref_lists as *const _
            } else {
                tracing::debug!("H265 no reference lists (IDR/I frame)");
                ptr::null()
            };

            // Build the Vulkan StdVideoH265ShortTermRefPicSet from the DPB
            // pipeline's InitializeRpsResult.
            if let Some(ref rps_res) = h265_rps_result {
                let rps = &rps_res.short_term_ref_pic_set;
                let mut strps_flags: vk::video::StdVideoH265ShortTermRefPicSetFlags =
                    std::mem::zeroed();
                if rps.inter_ref_pic_set_prediction_flag {
                    strps_flags.set_inter_ref_pic_set_prediction_flag(1);
                }

                tracing::debug!(
                    num_negative_pics = rps.num_negative_pics,
                    num_positive_pics = rps.num_positive_pics,
                    used_by_curr_pic_s0_flag = rps.used_by_curr_pic_s0_flag,
                    used_by_curr_pic_s1_flag = rps.used_by_curr_pic_s1_flag,
                    inter_ref_pic_set_prediction_flag = rps.inter_ref_pic_set_prediction_flag,
                    "H265 StdVideoH265ShortTermRefPicSet"
                );
                for i in 0..rps.num_negative_pics as usize {
                    tracing::debug!(
                        idx = i,
                        delta_poc_s0_minus1 = rps.delta_poc_s0_minus1[i],
                        used = (rps.used_by_curr_pic_s0_flag >> i) & 1,
                        "  STRPS negative delta (Vulkan struct)"
                    );
                }

                h265_short_term_ref_pic_set = vk::video::StdVideoH265ShortTermRefPicSet {
                    flags: strps_flags,
                    delta_idx_minus1: 0,
                    use_delta_flag: 0,
                    abs_delta_rps_minus1: 0,
                    used_by_curr_pic_flag: 0,
                    used_by_curr_pic_s0_flag: rps.used_by_curr_pic_s0_flag,
                    used_by_curr_pic_s1_flag: rps.used_by_curr_pic_s1_flag,
                    reserved1: 0,
                    reserved2: 0,
                    reserved3: 0,
                    num_negative_pics: rps.num_negative_pics,
                    num_positive_pics: rps.num_positive_pics,
                    delta_poc_s0_minus1: rps.delta_poc_s0_minus1,
                    delta_poc_s1_minus1: rps.delta_poc_s1_minus1,
                };

                // Set SPS flag and index from DPB pipeline
                if rps_res.short_term_ref_pic_set_sps_flag {
                    std_flags.set_short_term_ref_pic_set_sps_flag(1);
                }
            } else {
                h265_short_term_ref_pic_set = std::mem::zeroed::<
                    vk::video::StdVideoH265ShortTermRefPicSet,
                >();
            }

            let short_term_ref_pic_set_idx = h265_rps_result
                .as_ref()
                .map(|r| r.short_term_ref_pic_set_idx)
                .unwrap_or(0);

            tracing::debug!(
                pic_type = ?pic_type,
                PicOrderCntVal = h265_poc,
                short_term_ref_pic_set_sps_flag = std_flags.short_term_ref_pic_set_sps_flag(),
                short_term_ref_pic_set_idx = short_term_ref_pic_set_idx,
                is_reference = std_flags.is_reference(),
                IrapPicFlag = std_flags.IrapPicFlag(),
                pic_output_flag = std_flags.pic_output_flag(),
                no_output_of_prior_pics_flag = std_flags.no_output_of_prior_pics_flag(),
                has_ref_lists = h265_ref_list_result.is_some(),
                "H265 StdVideoEncodeH265PictureInfo"
            );

            h265_std_pic_info = vk::video::StdVideoEncodeH265PictureInfo {
                flags: std_flags,
                pic_type,
                sps_video_parameter_set_id: 0,
                pps_seq_parameter_set_id: 0,
                pps_pic_parameter_set_id: 0,
                short_term_ref_pic_set_idx,
                PicOrderCntVal: h265_poc,
                TemporalId: 0,
                reserved1: [0; 7],
                pRefLists: ref_lists_ptr,
                pShortTermRefPicSet: &h265_short_term_ref_pic_set,
                pLongTermRefPics: ptr::null(),
            };

            let qp = if config.rate_control_mode == RateControlMode::Cqp {
                match frame_type {
                    FrameType::Idr | FrameType::I => config.const_qp_intra,
                    FrameType::P => config.const_qp_inter_p,
                    FrameType::B => config.const_qp_inter_b,
                }
            } else {
                0
            };

            h265_nalu_slice = vk::VideoEncodeH265NaluSliceSegmentInfoKHR::builder()
                .constant_qp(qp)
                .std_slice_segment_header(&h265_slice_header);

            h265_pic_info = vk::VideoEncodeH265PictureInfoKHR::builder()
                .nalu_slice_segment_entries(std::slice::from_ref(&h265_nalu_slice))
                .std_picture_info(&h265_std_pic_info);

            encode_info = encode_info.push_next(&mut h265_pic_info);
        }

        // --- Begin query ---
        device.cmd_begin_query(
            self.command_buffer,
            self.query_pool,
            0,
            vk::QueryControlFlags::empty(),
        );

        // --- vkCmdEncodeVideoKHR ---
        device.cmd_encode_video_khr(self.command_buffer, &encode_info);

        // --- End query ---
        device.cmd_end_query(self.command_buffer, self.query_pool, 0);

        // --- vkCmdEndVideoCodingKHR ---
        let end_coding_info = vk::VideoEndCodingInfoKHR::default();
        device.cmd_end_video_coding_khr(self.command_buffer, &end_coding_info);

        // --- End command buffer ---
        device.end_command_buffer(self.command_buffer)?;

        // --- Submit to queue ---
        let command_buffers = [self.command_buffer];
        let submit_info = vk::SubmitInfo::builder().command_buffers(&command_buffers);

        device.reset_fences(&[self.fence])?;
        device.queue_submit(self.encode_queue, &[submit_info], self.fence)?;
        device.wait_for_fences(&[self.fence], true, u64::MAX)?;

        // --- Query encode feedback ---
        let mut feedback = EncodeFeedback::default();
        let feedback_bytes = std::slice::from_raw_parts_mut(
            &mut feedback as *mut EncodeFeedback as *mut u8,
            std::mem::size_of::<EncodeFeedback>(),
        );

        device.get_query_pool_results(
            self.query_pool,
            0,
            1,
            feedback_bytes,
            std::mem::size_of::<EncodeFeedback>() as u64,
            vk::QueryResultFlags::WAIT,
        )?;

        let offset = feedback.bitstream_offset as usize;
        let size = feedback.bitstream_bytes_written as usize;

        tracing::debug!(offset = offset, size = size, "Encode feedback");
        if is_h265 {
            tracing::debug!(
                frame_count = self.frame_count,
                frame_type = frame_type.name(),
                poc = h265_poc,
                bitstream_size = size,
                setup_slot = setup_slot_index,
                ref_count = reference_slot_indices.len(),
                "H265 encode_frame COMPLETE"
            );
        }

        // --- Read back bitstream data ---
        let mut data = vec![0u8; size];
        if size > 0 && !self.bitstream_mapped_ptr.is_null() {
            ptr::copy_nonoverlapping(
                self.bitstream_mapped_ptr.add(offset),
                data.as_mut_ptr(),
                size,
            );
        }

        // --- Update DPB slot ---
        // Only reference frames (IDR/I/P) persist in the DPB.
        // B-frames are non-reference: the slot can be reused immediately.
        self.dpb_slots[setup_slot_index].in_use = is_reference;
        // Record the picture type so references carry the correct type
        // (e.g. IDR references are marked as IDR, not P).
        self.dpb_slots[setup_slot_index].pic_type = match frame_type {
            FrameType::Idr => vk::video::STD_VIDEO_H264_PICTURE_TYPE_IDR,
            FrameType::I   => vk::video::STD_VIDEO_H264_PICTURE_TYPE_I,
            FrameType::P   => vk::video::STD_VIDEO_H264_PICTURE_TYPE_P,
            FrameType::B   => vk::video::STD_VIDEO_H264_PICTURE_TYPE_B,
        };
        self.dpb_slots[setup_slot_index].h265_pic_type = h265_setup_pic_type;
        if is_h265 && self.h265_encoder.is_some() {
            // H.265: use DPB-managed POC
            self.dpb_slots[setup_slot_index].frame_num = self.frame_count;
            self.dpb_slots[setup_slot_index].poc = h265_poc;
        } else if is_h264 && self.h264_encoder.is_some() {
            // H.264: use DPB-provided frame_num and POC
            self.dpb_slots[setup_slot_index].frame_num = h264_frame_num as u64;
            self.dpb_slots[setup_slot_index].poc = h264_poc;

            // Advance H.264-specific counters via VkVideoEncoderH264
            let enc = self.h264_encoder.as_mut().unwrap();
            if is_reference {
                let max_frame_num = 1u32 << (enc.log2_max_frame_num_minus4 + 4);
                enc.frame_num = (enc.frame_num + 1) % max_frame_num;
            }
            let max_poc_lsb = 1i32 << (enc.log2_max_pic_order_cnt_lsb_minus4 + 4);
            enc.poc_lsb = (enc.poc_lsb + 2) % max_poc_lsb;
        } else {
            // Fallback: use poc_counter
            self.dpb_slots[setup_slot_index].frame_num = self.frame_count;
            self.dpb_slots[setup_slot_index].poc = if self.codec_flag == vk::VideoCodecOperationFlagsKHR::ENCODE_H265 {
                self.poc_counter as i32
            } else {
                (self.poc_counter * 2) as i32
            };
        }

        // --- Advance counters ---
        self.frame_count += 1;
        self.poc_counter += 1;
        self.encode_order_count += 1;

        Ok(EncodedOutput {
            data,
            frame_type,
            pts,
            encode_order,
            bitstream_offset: feedback.bitstream_offset,
            bitstream_size: feedback.bitstream_bytes_written,
        })
    }
}
