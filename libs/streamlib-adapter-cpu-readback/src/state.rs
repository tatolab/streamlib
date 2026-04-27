// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Per-surface adapter state: host `VkImage`, dedicated linear staging
//! `VkBuffer`, timeline semaphore, and acquire/release counters.

use std::sync::Arc;

use streamlib::adapter_support::{VulkanPixelBuffer, VulkanTimelineSemaphore};
use streamlib::core::rhi::StreamTexture;
use streamlib_adapter_abi::SurfaceId;
use vulkanalia::vk;

/// `VkImageLayout` enumerant. Stored as `i32` per the Vulkan spec.
///
/// Convert to `vk::ImageLayout` via `vk::ImageLayout::from_raw(layout.0)`.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub(crate) struct VulkanLayout(pub i32);

impl VulkanLayout {
    pub const GENERAL: Self = Self(vk::ImageLayout::GENERAL.as_raw());

    pub(crate) fn vk(self) -> vk::ImageLayout {
        vk::ImageLayout::from_raw(self.0)
    }
}

/// Inputs the host hands to
/// [`crate::CpuReadbackSurfaceAdapter::register_host_surface`].
///
/// The host allocates the texture (typically via
/// `GpuContext::acquire_render_target_dma_buf_image`) and an exportable
/// timeline semaphore (via `VulkanTimelineSemaphore::new_exportable`),
/// then registers them here. The adapter takes joint ownership and
/// allocates its own dedicated linear staging buffer sized to the
/// texture's pixel footprint.
pub struct HostSurfaceRegistration {
    pub texture: StreamTexture,
    pub timeline: Arc<VulkanTimelineSemaphore>,
    /// Initial layout the host left the image in after allocation.
    /// For freshly-allocated images this is typically
    /// `vk::ImageLayout::UNDEFINED` (raw value 0).
    pub initial_image_layout: i32,
    /// Bytes per pixel of the staging buffer. The adapter copies
    /// `width * height * bytes_per_pixel` bytes through the staging
    /// buffer; the customer receives a `&[u8]` of that length.
    /// BGRA8/RGBA8 → 4. NV12 / multi-plane formats are not supported
    /// in v1.
    pub bytes_per_pixel: u32,
}

/// Per-surface state held inside the adapter's
/// `Mutex<HashMap<SurfaceId, _>>`.
///
/// Each entry owns a dedicated `VulkanPixelBuffer` (a
/// HOST_VISIBLE/HOST_COHERENT linear `VkBuffer`) sized once at
/// registration. The same staging buffer is reused on every acquire —
/// per-acquire allocation would be far too expensive on the hot path,
/// and the surface's dimensions are immutable for its lifetime.
pub(crate) struct SurfaceState {
    #[allow(dead_code)] // surface_id is kept for tracing / debug output
    pub(crate) surface_id: SurfaceId,
    pub(crate) texture: StreamTexture,
    pub(crate) staging: Arc<VulkanPixelBuffer>,
    pub(crate) timeline: Arc<VulkanTimelineSemaphore>,
    pub(crate) current_layout: VulkanLayout,
    pub(crate) read_holders: u64,
    pub(crate) write_held: bool,
    pub(crate) current_release_value: u64,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) bytes_per_pixel: u32,
}

impl SurfaceState {
    pub(crate) fn next_release_value(&self) -> u64 {
        self.current_release_value + 1
    }

    pub(crate) fn buffer_byte_size(&self) -> u64 {
        (self.width as u64) * (self.height as u64) * (self.bytes_per_pixel as u64)
    }
}
