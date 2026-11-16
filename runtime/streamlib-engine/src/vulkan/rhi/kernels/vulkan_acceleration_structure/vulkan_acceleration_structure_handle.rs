// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The public acceleration-structure handle over its inner build.

use std::ffi::c_void;
use std::sync::Arc;

use vulkanalia::vk;

use crate::core::Result;

use super::{
    AccelerationStructureKind, HostVulkanDevice, TlasInstanceDesc, VulkanAccelerationStructureInner,
};

/// Acceleration-structure (BLAS or TLAS) RHI handle over an opaque pointer to
/// an `Arc<VulkanAccelerationStructureInner>`. Clone / Drop run
/// `Arc::increment_strong_count` / `Arc::decrement_strong_count`.
pub struct VulkanAccelerationStructure {
    /// Opaque handle to the host's `Arc<VulkanAccelerationStructureInner>`.
    pub(crate) handle: *const c_void,
    /// Cached AS kind discriminant (0 = BottomLevel, 1 = TopLevel).
    /// Matches `AccelerationStructureKind`'s ordering.
    pub(crate) cached_kind: u32,
    /// Reserved padding so the next 8-byte field stays aligned.
    pub(crate) _reserved_padding: u32,
    /// Cached device address of the AS.
    pub(crate) cached_device_address: u64,
    /// Cached storage size in bytes.
    pub(crate) cached_storage_size: u64,
}

// SAFETY: `handle` points at an `Arc<VulkanAccelerationStructureInner>`
// whose interior is Send+Sync (Vulkan handles + queue submissions are
// guarded by the host queue mutex; BLAS refs are owned `Arc`s).
unsafe impl Send for VulkanAccelerationStructure {}
unsafe impl Sync for VulkanAccelerationStructure {}

impl VulkanAccelerationStructure {
    /// Leaks an initial strong count via `Arc::into_raw` and caches the
    /// inner's kind, device address and storage size.
    pub(crate) fn from_arc_into_raw(arc: Arc<VulkanAccelerationStructureInner>) -> Self {
        let cached_kind = match arc.kind() {
            AccelerationStructureKind::BottomLevel => 0u32,
            AccelerationStructureKind::TopLevel => 1u32,
        };
        let cached_device_address = arc.device_address();
        let cached_storage_size = arc.storage_size();
        let handle = Arc::into_raw(arc) as *const c_void;
        Self {
            handle,
            cached_kind,
            _reserved_padding: 0,
            cached_device_address,
            cached_storage_size,
        }
    }

    /// Engine-internal borrow of the host-owned
    /// `VulkanAccelerationStructureInner`. **Panics if called from
    /// cdylib code.**
    pub(crate) fn host_inner(&self) -> &VulkanAccelerationStructureInner {
        // SAFETY: `self.handle` is `Arc::into_raw(Arc<VulkanAccelerationStructureInner>)`.
        unsafe { &*(self.handle as *const VulkanAccelerationStructureInner) }
    }

    pub fn build_triangles_blas(
        vulkan_device: &Arc<HostVulkanDevice>,
        label: &str,
        vertices: &[f32],
        indices: &[u32],
    ) -> Result<Self> {
        VulkanAccelerationStructureInner::build_triangles_blas(
            vulkan_device,
            label,
            vertices,
            indices,
        )
        .map(Self::from_arc_into_raw)
    }

    /// Build a TLAS from a list of TLAS instances. Engine-side entry.
    pub fn build_tlas(
        vulkan_device: &Arc<HostVulkanDevice>,
        label: &str,
        instances: &[TlasInstanceDesc],
    ) -> Result<Self> {
        VulkanAccelerationStructureInner::build_tlas(vulkan_device, label, instances)
            .map(Self::from_arc_into_raw)
    }

    /// `VkAccelerationStructureKHR` handle. **Engine-internal** — the
    /// vulkanalia handle layout couples to the vulkanalia minor
    /// version and isn't safe to surface across the plugin ABI.
    /// There is no in-tree cdylib consumer that reads this; every
    /// binding flows through the ray-tracing kernel's
    /// `set_acceleration_structure` slot, which dereferences the AS
    /// on the host side. Panics if called from cdylib code.
    pub(crate) fn vk_handle(&self) -> vk::AccelerationStructureKHR {
        self.host_inner().vk_handle()
    }

    /// Device address of the AS, cached by `from_arc_into_raw`.
    pub fn device_address(&self) -> u64 {
        self.cached_device_address
    }

    /// `BottomLevel` or `TopLevel`. Reads the cached POD value
    /// populated at mint time. The discriminant is 0 = `BottomLevel`,
    /// 1 = `TopLevel`; any other value is a corruption bug we
    /// surface as `BottomLevel` rather than panic (host-side mint
    /// paths are the source of truth and only ever write 0 or 1).
    pub fn kind(&self) -> AccelerationStructureKind {
        match self.cached_kind {
            0 => AccelerationStructureKind::BottomLevel,
            1 => AccelerationStructureKind::TopLevel,
            _ => AccelerationStructureKind::BottomLevel,
        }
    }

    /// Human-readable label used in diagnostics, read from the inner.
    pub fn label(&self) -> String {
        self.host_inner().label().to_string()
    }

    /// Storage size in bytes. Reads the cached POD value populated
    /// at mint time.
    pub fn storage_size(&self) -> vk::DeviceSize {
        self.cached_storage_size
    }
}

impl Clone for VulkanAccelerationStructure {
    fn clone(&self) -> Self {
        if !self.handle.is_null() {
            // SAFETY: `handle` is `Arc::into_raw(Arc<VulkanAccelerationStructureInner>)`;
            // the increment in `Clone` and this decrement are balanced.
            unsafe {
                Arc::increment_strong_count(self.handle as *const VulkanAccelerationStructureInner);
            }
        }
        Self {
            handle: self.handle,
            cached_kind: self.cached_kind,
            _reserved_padding: self._reserved_padding,
            cached_device_address: self.cached_device_address,
            cached_storage_size: self.cached_storage_size,
        }
    }
}

impl Drop for VulkanAccelerationStructure {
    fn drop(&mut self) {
        if !self.handle.is_null() {
            // SAFETY: `handle` is `Arc::into_raw(Arc<VulkanAccelerationStructureInner>)`;
            // the increment in `Clone` and this decrement are balanced.
            unsafe {
                Arc::decrement_strong_count(self.handle as *const VulkanAccelerationStructureInner);
            }
        }
    }
}

impl std::fmt::Debug for VulkanAccelerationStructure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VulkanAccelerationStructure").finish()
    }
}
