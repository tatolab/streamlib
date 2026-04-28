// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Per-surface adapter state: host `VkImage`, dedicated linear staging
//! `VkBuffer`s (one per plane), timeline semaphore, and acquire/release
//! counters.

use std::sync::Arc;

use streamlib::adapter_support::{HostVulkanPixelBuffer, HostVulkanTimelineSemaphore};
use streamlib::core::rhi::StreamTexture;
use streamlib_adapter_abi::{SurfaceFormat, SurfaceId, SurfaceRegistration};
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
/// timeline semaphore (via `HostVulkanTimelineSemaphore::new_exportable`),
/// then registers them here. The adapter takes joint ownership and
/// allocates one dedicated linear staging buffer per plane sized to the
/// plane's pixel footprint.
pub struct HostSurfaceRegistration {
    pub texture: StreamTexture,
    pub timeline: Arc<HostVulkanTimelineSemaphore>,
    /// Initial layout the host left the image in after allocation.
    /// For freshly-allocated images this is typically
    /// `vk::ImageLayout::UNDEFINED` (raw value 0).
    pub initial_image_layout: i32,
    /// Pixel format of the surface. Determines plane count and per-plane
    /// dimensions of the staging buffers (see [`SurfaceFormat::plane_count`]).
    pub format: SurfaceFormat,
}

/// Per-plane staging slot. One [`HostVulkanPixelBuffer`] per plane, with the
/// plane's tightly-packed `(width, height, bytes_per_pixel)` geometry
/// recorded so the copy paths and the customer-facing view can compute
/// strides without re-deriving from `format` on every access.
pub(crate) struct PlaneSlot {
    pub(crate) staging: Arc<HostVulkanPixelBuffer>,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) bytes_per_pixel: u32,
}

impl PlaneSlot {
    pub(crate) fn byte_size(&self) -> u64 {
        (self.width as u64) * (self.height as u64) * (self.bytes_per_pixel as u64)
    }
}

/// Per-surface state held inside the adapter's
/// `Mutex<HashMap<SurfaceId, _>>`.
///
/// Each entry owns one dedicated `HostVulkanPixelBuffer` per plane (a
/// HOST_VISIBLE/HOST_COHERENT linear `VkBuffer`) sized once at
/// registration. The staging buffers are reused on every acquire — per-
/// acquire allocation would be far too expensive on the hot path, and
/// the surface's dimensions are immutable for its lifetime.
pub(crate) struct SurfaceState {
    #[allow(dead_code)] // surface_id is kept for tracing / debug output
    pub(crate) surface_id: SurfaceId,
    pub(crate) texture: StreamTexture,
    pub(crate) planes: Vec<PlaneSlot>,
    pub(crate) timeline: Arc<HostVulkanTimelineSemaphore>,
    pub(crate) current_layout: VulkanLayout,
    pub(crate) read_holders: u64,
    pub(crate) write_held: bool,
    pub(crate) current_release_value: u64,
    pub(crate) format: SurfaceFormat,
    pub(crate) width: u32,
    pub(crate) height: u32,
}

impl SurfaceState {
    pub(crate) fn next_release_value(&self) -> u64 {
        self.current_release_value + 1
    }
}

impl SurfaceRegistration for SurfaceState {
    fn write_held(&self) -> bool {
        self.write_held
    }
    fn read_holders(&self) -> u64 {
        self.read_holders
    }
    fn set_write_held(&mut self, held: bool) {
        self.write_held = held;
    }
    fn inc_read_holders(&mut self) {
        self.read_holders += 1;
    }
    fn dec_read_holders(&mut self) {
        self.read_holders = self.read_holders.saturating_sub(1);
    }
}
