// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Per-type kernel binding traits.
//!
//! Each Vulkan binding slot (storage / uniform / vertex / index) has
//! its own trait, and each typed wrapper implements only the traits
//! matching its allocation's `VkBufferUsageFlags`. This is what makes
//! it impossible to bind a [`crate::core::rhi::PixelBuffer`] as a
//! vertex buffer at compile time — `PixelBuffer` does not implement
//! [`VulkanVertexBindable`].
//!
//! Implementation note: a single `VkBuffer` can carry multiple
//! `BufferUsageFlags` bits simultaneously (e.g. a buffer flagged
//! `VERTEX | INDEX | STORAGE` legitimately binds to any of those
//! slots). The trait taxonomy here enforces exclusion at the
//! **binding-site** layer; the allocation layer is free to combine
//! usage flags. If a future workflow needs a multi-usage buffer, the
//! typed wrappers can each provide a `wrap_existing` constructor
//! that asserts the underlying usage flag is present.

use vulkanalia::vk;

use crate::core::rhi::PixelBuffer;
#[cfg(target_os = "linux")]
use crate::core::rhi::{IndexBuffer, StorageBuffer, UniformBuffer, VertexBuffer};

/// Common shape returned by every binding trait — the kernel-internal
/// recorder only needs `(vk::Buffer, vk::DeviceSize)`.
#[doc(hidden)]
pub(super) fn vk_buffer_handle_for_pixel_buffer(
    buffer: &PixelBuffer,
) -> (vk::Buffer, vk::DeviceSize) {
    #[cfg(target_os = "linux")]
    {
        let inner = &buffer.buffer_ref().inner;
        (inner.buffer(), inner.size())
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = buffer;
        (vk::Buffer::null(), 0)
    }
}

/// Buffers bindable as a Vulkan **storage buffer** (SSBO).
///
/// Implemented by [`PixelBuffer`] (pixel-data-as-SSBO is legitimate;
/// `PixelBuffer` allocations carry `STORAGE_BUFFER` usage from birth)
/// and [`StorageBuffer`] (the canonical raw-bytes shape from
/// [`crate::core::context::GpuContext::acquire_storage_buffer`]).
pub trait VulkanStorageBindable {
    fn vk_buffer(&self) -> vk::Buffer;
    fn vk_buffer_size(&self) -> vk::DeviceSize;
}

/// Buffers bindable as a Vulkan **uniform buffer** (UBO).
///
/// Implemented only by [`UniformBuffer`]. Pixel buffers cannot be bound
/// as UBOs because their allocations do not carry `UNIFORM_BUFFER`
/// usage.
///
/// ```compile_fail,E0277
/// use streamlib_engine::host_rhi::VulkanUniformBindable;
/// use streamlib_engine::core::rhi::PixelBuffer;
/// fn accepts_uniform<B: VulkanUniformBindable>(_: &B) {}
/// fn rejected(pb: &PixelBuffer) {
///     // PixelBuffer does NOT implement VulkanUniformBindable —
///     // its allocations don't carry UNIFORM_BUFFER usage.
///     accepts_uniform(pb);
/// }
/// ```
pub trait VulkanUniformBindable {
    fn vk_buffer(&self) -> vk::Buffer;
    fn vk_buffer_size(&self) -> vk::DeviceSize;
}

/// Buffers bindable as a **vertex input** buffer.
///
/// Implemented only by [`VertexBuffer`].
///
/// ```compile_fail,E0277
/// use streamlib_engine::host_rhi::VulkanVertexBindable;
/// use streamlib_engine::core::rhi::PixelBuffer;
/// fn accepts_vertex<B: VulkanVertexBindable>(_: &B) {}
/// fn rejected(pb: &PixelBuffer) {
///     // PixelBuffer does NOT implement VulkanVertexBindable.
///     accepts_vertex(pb);
/// }
/// ```
pub trait VulkanVertexBindable {
    fn vk_buffer(&self) -> vk::Buffer;
    fn vk_buffer_size(&self) -> vk::DeviceSize;
}

/// Buffers bindable as an **index** buffer.
///
/// Implemented only by [`IndexBuffer`].
///
/// ```compile_fail,E0277
/// use streamlib_engine::host_rhi::VulkanIndexBindable;
/// use streamlib_engine::core::rhi::PixelBuffer;
/// fn accepts_index<B: VulkanIndexBindable>(_: &B) {}
/// fn rejected(pb: &PixelBuffer) {
///     // PixelBuffer does NOT implement VulkanIndexBindable.
///     accepts_index(pb);
/// }
/// ```
pub trait VulkanIndexBindable {
    fn vk_buffer(&self) -> vk::Buffer;
    fn vk_buffer_size(&self) -> vk::DeviceSize;
}

// --- Storage bindings ---

impl VulkanStorageBindable for PixelBuffer {
    fn vk_buffer(&self) -> vk::Buffer {
        vk_buffer_handle_for_pixel_buffer(self).0
    }
    fn vk_buffer_size(&self) -> vk::DeviceSize {
        vk_buffer_handle_for_pixel_buffer(self).1
    }
}

#[cfg(target_os = "linux")]
impl VulkanStorageBindable for StorageBuffer {
    fn vk_buffer(&self) -> vk::Buffer {
        self.host_inner().buffer()
    }
    fn vk_buffer_size(&self) -> vk::DeviceSize {
        self.host_inner().size()
    }
}

// --- Uniform bindings ---

#[cfg(target_os = "linux")]
impl VulkanUniformBindable for UniformBuffer {
    fn vk_buffer(&self) -> vk::Buffer {
        self.host_inner().buffer()
    }
    fn vk_buffer_size(&self) -> vk::DeviceSize {
        self.host_inner().size()
    }
}

// --- Vertex bindings ---

#[cfg(target_os = "linux")]
impl VulkanVertexBindable for VertexBuffer {
    fn vk_buffer(&self) -> vk::Buffer {
        self.host_inner().buffer()
    }
    fn vk_buffer_size(&self) -> vk::DeviceSize {
        self.host_inner().size()
    }
}

// --- Index bindings ---

#[cfg(target_os = "linux")]
impl VulkanIndexBindable for IndexBuffer {
    fn vk_buffer(&self) -> vk::Buffer {
        self.host_inner().buffer()
    }
    fn vk_buffer_size(&self) -> vk::DeviceSize {
        self.host_inner().size()
    }
}
