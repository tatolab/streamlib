// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Engine-internal trait that lets command-buffer recording methods on
//! [`RhiCommandRecorder`](super::RhiCommandRecorder) accept any of
//! streamlib's typed buffer wrappers ([`PixelBuffer`],
//! [`StorageBuffer`], [`UniformBuffer`], [`VertexBuffer`],
//! [`IndexBuffer`]) or a raw [`HostVulkanBuffer`] uniformly. The raw
//! variant is needed because some allocation flavors — notably
//! OPAQUE_FD-exportable buffers used in CUDA / OpenCL interop — have
//! no typed wrapper above [`HostVulkanBuffer`] but still participate
//! in transfer + barrier recording.
//!
//! Distinct from the binding-site traits in
//! [`vulkan_storage_binding`](super::vulkan_storage_binding) (which gate
//! kernel-side `set_*_buffer` slot type-safety). [`VulkanBufferLike`]
//! only exposes the underlying `(vk::Buffer, vk::DeviceSize)` for
//! transfer + barrier recording — slot semantics don't apply.
//!
//! [`PixelBuffer`]: crate::core::rhi::PixelBuffer
//! [`StorageBuffer`]: crate::core::rhi::StorageBuffer
//! [`UniformBuffer`]: crate::core::rhi::UniformBuffer
//! [`VertexBuffer`]: crate::core::rhi::VertexBuffer
//! [`IndexBuffer`]: crate::core::rhi::IndexBuffer
//! [`HostVulkanBuffer`]: super::HostVulkanBuffer

use vulkanalia::vk;

use crate::core::rhi::PixelBuffer;
#[cfg(target_os = "linux")]
use crate::core::rhi::{IndexBuffer, StorageBuffer, UniformBuffer, VertexBuffer};

/// Any of streamlib's typed buffer wrappers, projected onto the raw
/// `(vk::Buffer, vk::DeviceSize)` pair the recorder needs.
pub trait VulkanBufferLike {
    fn vk_buffer(&self) -> vk::Buffer;
    fn vk_buffer_size(&self) -> vk::DeviceSize;

    /// Cdylib-mode handle accessor: if this buffer flavor is
    /// reachable as a [`crate::core::rhi::StorageBuffer`] PluginAbiObject,
    /// return the underlying `Arc::into_raw(Arc<HostVulkanBufferInner>)`
    /// pointer for plugin ABI dispatch (Phase E sub-lift slice B —
    /// #984). Default is `None`; only [`crate::core::rhi::StorageBuffer`]
    /// overrides today. The cdylib-side
    /// [`crate::vulkan::rhi::RhiCommandRecorder::record_buffer_barrier`]
    /// / `record_copy_image_to_buffer` paths route to the
    /// StorageBuffer-flavored vtable slot when this returns `Some`.
    fn cdylib_storage_buffer_handle(&self) -> Option<*const std::ffi::c_void> {
        None
    }

    /// Cdylib-mode handle accessor for the [`PixelBuffer`] PluginAbiObject
    /// (issue #988 sibling-slot extension of Phase E sub-lift slice
    /// B). Returns the underlying
    /// `Arc::into_raw(Arc<PixelBufferRef>)` pointer when this buffer
    /// flavor is a [`PixelBuffer`]. The cdylib-side
    /// `record_buffer_barrier` / `record_copy_image_to_buffer` paths
    /// route to the PixelBuffer-flavored vtable slot
    /// (`record_pixel_buffer_barrier`,
    /// `record_copy_image_to_pixel_buffer`) when this returns `Some`.
    ///
    /// At most one of [`Self::cdylib_storage_buffer_handle`] /
    /// [`Self::cdylib_pixel_buffer_handle`] returns `Some` for any
    /// given implementor; the recorder's dispatch logic checks both
    /// in order and errors with a typed "unsupported buffer flavor"
    /// message when neither matches, matching the slot coverage of
    /// [`streamlib_plugin_abi::RhiCommandRecorderMethodsVTable`].
    ///
    /// [`PixelBuffer`]: crate::core::rhi::PixelBuffer
    fn cdylib_pixel_buffer_handle(&self) -> Option<*const std::ffi::c_void> {
        None
    }
}

impl VulkanBufferLike for PixelBuffer {
    fn vk_buffer(&self) -> vk::Buffer {
        #[cfg(target_os = "linux")]
        {
            self.buffer_ref().inner.buffer()
        }
        #[cfg(not(target_os = "linux"))]
        {
            vk::Buffer::null()
        }
    }

    fn vk_buffer_size(&self) -> vk::DeviceSize {
        #[cfg(target_os = "linux")]
        {
            self.buffer_ref().inner.size()
        }
        #[cfg(not(target_os = "linux"))]
        {
            0
        }
    }

    fn cdylib_pixel_buffer_handle(&self) -> Option<*const std::ffi::c_void> {
        Some(self.handle)
    }
}

#[cfg(target_os = "linux")]
impl VulkanBufferLike for StorageBuffer {
    fn vk_buffer(&self) -> vk::Buffer {
        self.host_inner().buffer()
    }
    fn vk_buffer_size(&self) -> vk::DeviceSize {
        self.host_inner().size()
    }
    fn cdylib_storage_buffer_handle(&self) -> Option<*const std::ffi::c_void> {
        Some(self.cdylib_handle())
    }
}

#[cfg(target_os = "linux")]
impl VulkanBufferLike for UniformBuffer {
    fn vk_buffer(&self) -> vk::Buffer {
        self.host_inner().buffer()
    }
    fn vk_buffer_size(&self) -> vk::DeviceSize {
        self.host_inner().size()
    }
}

#[cfg(target_os = "linux")]
impl VulkanBufferLike for VertexBuffer {
    fn vk_buffer(&self) -> vk::Buffer {
        self.host_inner().buffer()
    }
    fn vk_buffer_size(&self) -> vk::DeviceSize {
        self.host_inner().size()
    }
}

#[cfg(target_os = "linux")]
impl VulkanBufferLike for IndexBuffer {
    fn vk_buffer(&self) -> vk::Buffer {
        self.host_inner().buffer()
    }
    fn vk_buffer_size(&self) -> vk::DeviceSize {
        self.host_inner().size()
    }
}

#[cfg(target_os = "linux")]
impl VulkanBufferLike for crate::vulkan::rhi::HostVulkanBuffer {
    fn vk_buffer(&self) -> vk::Buffer {
        self.buffer()
    }
    fn vk_buffer_size(&self) -> vk::DeviceSize {
        self.size()
    }
}
