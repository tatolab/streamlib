// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

impl GpuContextFullAccess {
    /// Allocate a render-target-capable VkImage over a private IOSurface
    /// (privileged path). See
    /// [`GpuContext::acquire_render_target_iosurface_image`].
    /// Counted against this node's one-offs of its shape; see
    /// [`OneOffAcquisitionCountedForItsNode`].
    pub fn acquire_render_target_iosurface_image(
        &self,
        width: u32,
        height: u32,
        format: TextureFormat,
    ) -> Result<OneOffAcquisitionCountedForItsNode<Texture>> {
        acquire_a_one_off_counted_against_its_node(
            self.one_off_acquisitions_of_the_node.as_ref(),
            OneOffAcquisitionShape::RenderTargetImage {
                width,
                height,
                format,
            },
            || {
                self.host_inner()
                    .acquire_render_target_iosurface_image(width, height, format)
            },
        )
    }

    /// Import an IOSurface's memory as a storage buffer, zero-copy. See
    /// [`GpuContext::import_iosurface_as_storage_buffer`].
    ///
    /// FullAccess-only: every import creates a buffer and imports memory.
    pub fn import_iosurface_as_storage_buffer(
        &self,
        iosurface: &objc2_io_surface::IOSurfaceRef,
    ) -> Result<crate::vulkan::rhi::ImportedIOSurfaceStorageBuffer> {
        self.host_inner()
            .import_iosurface_as_storage_buffer(iosurface)
    }
}
