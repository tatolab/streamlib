// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The Vulkan buffer's tests, split by concern, and the imports they share.

use super::*;

use vulkanalia::vk;

use crate::core::Error;

use crate::vulkan::rhi::HostVulkanDevice;

#[cfg(target_os = "linux")]
mod dma_buf_import_tests;
mod host_visible_and_storage_buffer_tests;
#[cfg(target_os = "linux")]
mod opaque_fd_and_dma_buf_export_tests;
