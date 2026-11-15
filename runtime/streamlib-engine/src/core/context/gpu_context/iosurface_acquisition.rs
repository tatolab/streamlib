// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

impl GpuContext {
    /// Allocate a render-target-capable VkImage over a fresh private
    /// IOSurface — the macOS peer of
    /// [`Self::acquire_render_target_dma_buf_image`], with the same usage
    /// set. Single-plane formats only.
    pub fn acquire_render_target_iosurface_image(
        &self,
        width: u32,
        height: u32,
        format: TextureFormat,
    ) -> Result<Texture> {
        tracing::debug!(
            rhi_op = "acquire_render_target_iosurface_image",
            width,
            height,
            format = ?format,
            "GpuContext::acquire_render_target_iosurface_image"
        );
        let desc = TextureDescriptor::new(width, height, format)
            .with_usage(render_target_texture_usages());
        self.device.create_texture_iosurface_backed(&desc)
    }

    /// Import an IOSurface's memory as a storage buffer, zero-copy, retaining
    /// the surface for the buffer's life. Refused, naming the reason, when
    /// the device cannot import host memory, the surface's base address is
    /// off the import alignment, or the driver declines (MoltenVK before
    /// 1.4.1 does). Backs
    /// [`GpuContextFullAccess::import_iosurface_as_storage_buffer`].
    #[tracing::instrument(
        level = "debug",
        skip(self, iosurface),
        fields(rhi_op = "import_iosurface_as_storage_buffer")
    )]
    pub fn import_iosurface_as_storage_buffer(
        &self,
        iosurface: &objc2_io_surface::IOSurfaceRef,
    ) -> Result<crate::vulkan::rhi::ImportedIOSurfaceStorageBuffer> {
        crate::vulkan::rhi::ImportedIOSurfaceStorageBuffer::import(&self.device.inner, iosurface)
    }
}
