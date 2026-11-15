// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::sync::Arc;

use crate::core::Result;
use crate::core::rhi::TextureDescriptor;
use crate::vulkan::rhi::HostVulkanTexture;

use super::HostVulkanDevice;

impl HostVulkanDevice {
    /// Create a texture on this device.
    pub fn create_texture(self: &Arc<Self>, desc: &TextureDescriptor) -> Result<HostVulkanTexture> {
        HostVulkanTexture::new(self, desc)
    }

    /// Create a non-exportable device-local texture for same-process consumers.
    ///
    /// Unlike [`create_texture`], this skips the DMA-BUF export pool and
    /// allocates from the default VMA pool. Use this for textures that never
    /// cross process boundaries — NVIDIA Linux caps DMA-BUF exportable
    /// allocations after swapchain creation, so minimizing exportable
    /// allocations is important.
    pub fn create_texture_local(
        self: &Arc<Self>,
        desc: &TextureDescriptor,
    ) -> Result<HostVulkanTexture> {
        HostVulkanTexture::new_device_local(self, desc)
    }
}
