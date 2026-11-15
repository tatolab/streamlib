// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A macOS buffer over an IOSurface.

use super::HostVulkanBuffer;

impl HostVulkanBuffer {
    /// Record the IOSurface whose pages this host-pointer import aliases; the
    /// buffer keeps it alive past the memory importing it.
    pub(in crate::vulkan::rhi) fn backed_by_iosurface(
        mut self,
        iosurface: crate::apple::iosurface::RetainedIOSurfaceSharedAcrossThreads,
    ) -> Self {
        self.backing_iosurface = Some(iosurface);
        self
    }

    /// The IOSurface this buffer's memory is, when it is one.
    pub fn backing_iosurface(&self) -> Option<&objc2_io_surface::IOSurfaceRef> {
        self.backing_iosurface.as_deref()
    }
}
