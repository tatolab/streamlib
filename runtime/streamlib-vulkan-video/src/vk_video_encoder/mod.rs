// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! VkVideoEncoder — Vulkan Video encode pipeline.
//!
//! Port of nvpro-samples/vk_video_encoder/libs/VkVideoEncoder/

// --- Batch 1: definitions, GOP, config, PSNR, state ---
pub mod vk_encoder_config;
pub mod vk_encoder_config_av1;
pub mod vk_encoder_config_h264;
pub mod vk_encoder_config_h265;
pub mod vk_video_encoder_def;
pub mod vk_video_encoder_psnr;
pub mod vk_video_encoder_state_h264;
pub mod vk_video_encoder_state_h265;
pub mod vk_video_gop_structure;

// --- Batch 2: encoder pipeline, codec-specific encoders, DPB, state ---
pub mod vk_encoder_dpb_av1;
pub mod vk_encoder_dpb_h264;
pub mod vk_encoder_dpb_h265;
pub mod vk_encoder_h264;
#[expect(
    clippy::module_inception,
    reason = "module names follow the nvpro-samples sources they port"
)]
pub mod vk_video_encoder;
pub mod vk_video_encoder_av1;
pub mod vk_video_encoder_h264;
pub mod vk_video_encoder_h265;
pub mod vk_video_encoder_state_av1;
