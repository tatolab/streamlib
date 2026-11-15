// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The Vulkan texture's tests, split by concern, and the imports they share.

use super::*;

use crate::core::rhi::TextureDescriptor;

#[cfg(target_os = "linux")]
mod dma_buf_plane_layout_export_and_render_target_tests;
#[cfg(target_os = "macos")]
mod macos_iosurface_backed_texture_tests;
#[cfg(target_os = "linux")]
mod opaque_fd_export_and_import_tests;
mod texture_creation_lifetime_and_image_view_tests;
