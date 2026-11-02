// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! NvVideoParser — Vulkan Video bitstream parser library.
//!
//! Port of nvpro-samples/vk_video_decoder/libs/NvVideoParser/

pub mod byte_stream_parser;
pub mod nv_vulkan_h264_scaling_list;
pub mod nv_vulkan_h265_scaling_list;
#[allow(dead_code, unused_variables, unused_assignments, unused_mut)]
pub mod vulkan_av1_decoder;
pub mod vulkan_h264_decoder;
pub mod vulkan_h265_decoder;
pub mod vulkan_h26x_decoder;
pub mod vulkan_video_decoder;
pub mod vulkan_vp9_decoder;
