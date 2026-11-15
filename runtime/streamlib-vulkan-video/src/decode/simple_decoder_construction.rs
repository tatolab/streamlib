// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Minting a [`SimpleDecoder`] on a codec device: its queues, the decode context and the transfer
//! resources it reads pictures back with.

use std::sync::Arc;
use tracing::info;
use vulkanalia::prelude::v1_4::*;
use vulkanalia::vk;

use super::SimpleDecoder;
use super::exported_nv12_dma_buf_ring;
use super::types::*;
use crate::VideoError;
use crate::VulkanVideoDecodeContext;
use crate::nv_video_parser::vulkan_h264_decoder::MAX_DPB_SIZE as H264_MAX_DPB_SIZE;
use crate::nv_video_parser::vulkan_h265_decoder::HEVC_DPB_SIZE;

impl SimpleDecoder {
    /// Mint a decoder on `codec_device`'s video decode queue, staging
    /// readbacks on its transfer queue and converting NV12 → RGBA on its
    /// compute queue.
    ///
    /// # Errors
    ///
    /// Refused when the device enabled no Vulkan Video decode queue, or any
    /// Vulkan resource creation fails.
    pub fn new(
        codec_device: Arc<dyn crate::VulkanVideoCodecDevice>,
        config: SimpleDecoderConfig,
    ) -> Result<Self, VideoError> {
        let crate::VulkanVideoCodecQueue {
            queue: decode_queue,
            queue_family_index: decode_queue_family,
        } = codec_device.video_decode_queue().ok_or_else(|| {
            VideoError::BitstreamError(
                "the device enabled no video decode queue — it does not support Vulkan \
                 Video decode"
                    .into(),
            )
        })?;
        let crate::VulkanVideoCodecQueue {
            queue: transfer_queue,
            queue_family_index: transfer_queue_family,
        } = codec_device.transfer_queue();
        let crate::VulkanVideoCodecQueue {
            queue: compute_queue,
            queue_family_index: compute_queue_family,
        } = codec_device.compute_queue();
        let device = codec_device.device().clone();
        let ctx = Arc::new(VulkanVideoDecodeContext::new(Arc::clone(&codec_device))?);

        let transfer_pool = unsafe {
            device
                .create_command_pool(
                    &vk::CommandPoolCreateInfo::builder()
                        .queue_family_index(transfer_queue_family)
                        .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER),
                    None,
                )
                .map_err(VideoError::from)?
        };

        let transfer_cb = unsafe {
            device
                .allocate_command_buffers(
                    &vk::CommandBufferAllocateInfo::builder()
                        .command_pool(transfer_pool)
                        .level(vk::CommandBufferLevel::PRIMARY)
                        .command_buffer_count(1),
                )
                .map_err(VideoError::from)?[0]
        };

        let transfer_fence = unsafe {
            device
                .create_fence(&vk::FenceCreateInfo::default(), None)
                .map_err(VideoError::from)?
        };

        info!(codec = ?config.codec, "SimpleDecoder created (external device)");
        let exported_nv12_dma_buf_ring =
            exported_nv12_dma_buf_ring::ExportedNv12DmaBufRing::new(device.clone());

        Ok(Self {
            device,
            vk_decoder: None,
            decode_queue,
            decode_queue_family,
            transfer_queue,
            transfer_queue_family,
            transfer_pool,
            transfer_cb,
            transfer_fence,
            nal_buffer: Vec::new(),
            cached_vps_nalu: None,
            cached_sps_nalu: None,
            cached_pps_nalu: None,
            coded_picture_width: 0,
            coded_picture_height: 0,
            decoded_picture_display_window: None,
            session_configured: false,
            frame_counter: 0,
            frame_num: 0,
            idr_pic_id: 0,
            dpb_slot_in_use: Vec::new(),
            dpb_slot_frame_num: Vec::new(),
            dpb_slot_poc: Vec::new(),
            config,
            ctx,
            h264_parser: None,
            h264_dpb_to_slot: [-1i32; H264_MAX_DPB_SIZE + 1],
            h265_parser: None,
            h265_dpb_to_slot: [-1i32; HEVC_DPB_SIZE],
            readback_staging: None,
            exported_nv12_dma_buf_ring,
            pending_frame: None,
            nv12_converter: None,
            rgba_staging: None,
            compute_queue,
            compute_queue_family,
            codec_device,
        })
    }
}
