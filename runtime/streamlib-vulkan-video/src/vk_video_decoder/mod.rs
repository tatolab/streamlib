// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: GPL-3.0-only

//! VkVideoDecoder — Vulkan Video decode pipeline.
//!
//! Port of nvpro-samples/vk_video_decoder/libs/VkVideoDecoder/

#[allow(dead_code)]
pub mod vk_parser_video_picture_parameters;
#[allow(dead_code)]
#[expect(
    clippy::module_inception,
    reason = "module names follow the nvpro-samples sources they port"
)]
pub mod vk_video_decoder;
