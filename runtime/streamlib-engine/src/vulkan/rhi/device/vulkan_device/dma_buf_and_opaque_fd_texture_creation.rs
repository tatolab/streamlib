// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::sync::Arc;

use crate::core::rhi::TextureDescriptor;
use crate::core::{Error, Result};
use crate::vulkan::rhi::HostVulkanTexture;
use crate::vulkan::rhi::device::drm_modifier_probe;

use super::HostVulkanDevice;

impl HostVulkanDevice {
    /// Create an OPAQUE_FD-exportable texture whose memory a foreign device
    /// (a helper child's Vulkan import, CUDA) can adopt whole.
    ///
    /// Thin wrapper over [`HostVulkanTexture::new_opaque_fd_export`]; that
    /// constructor's usage and format rules apply.
    pub fn create_texture_opaque_fd_export(
        self: &Arc<Self>,
        desc: &TextureDescriptor,
    ) -> Result<HostVulkanTexture> {
        HostVulkanTexture::new_opaque_fd_export(self, desc)
    }

    /// Create a DMA-BUF-exportable texture with an explicit DRM format
    /// modifier, importable by a foreign process as the same tiled image.
    ///
    /// The modifier candidates come from this device's EGL probe
    /// (`external_only=FALSE` — render-target-capable). Errors when the
    /// format has no DRM FOURCC or the probe advertised no RT-capable
    /// modifier for it — there is no fallback to LINEAR (sampler-only on
    /// NVIDIA, whose EGL samples linear DMA-BUFs only).
    pub fn create_texture_render_target_dma_buf(
        self: &Arc<Self>,
        desc: &TextureDescriptor,
    ) -> Result<HostVulkanTexture> {
        let Some(fourcc) = drm_modifier_probe::fourcc::drm_fourcc_for_texture_format(desc.format)
        else {
            return Err(Error::GpuError(format!(
                "create_texture_render_target_dma_buf: format {:?} has no DRM FOURCC \
                 mapping; it cannot cross a process boundary as a DMA-BUF image",
                desc.format
            )));
        };
        let modifiers: Vec<u64> = self.drm_modifier_table().rt_modifiers(fourcc).to_vec();
        if modifiers.is_empty() {
            return Err(Error::GpuError(format!(
                "create_texture_render_target_dma_buf: no RT-capable DRM modifier for \
                 {:?} (fourcc=0x{fourcc:08x}); EGL probe returned an empty list",
                desc.format
            )));
        }
        HostVulkanTexture::new_render_target_dma_buf(self, desc, &modifiers)
    }
}
