// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Color conversion, tone mapping and offscreen compositing of frames.

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(super) mod rgba_or_bgra_image_to_rgba_image_conversion;
pub(super) mod vulkan_color_converter;
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(super) mod vulkan_offscreen_compositor;
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(super) mod vulkan_tone_mapper;
