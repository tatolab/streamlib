// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use crate::vulkan::rhi::vulkan_kernel_capability_refusal::{
    VulkanSubgroupOperationSupport, ray_tracing_tier_absent_refusal,
};

use super::{HostVulkanDevice, RayTracingPipelineProperties};

impl HostVulkanDevice {
    /// Get the graphics queue family index.
    pub fn queue_family_index(&self) -> u32 {
        self.queue_family_index
    }

    /// Get the dedicated transfer queue family index (falls back to graphics queue).
    pub fn transfer_queue_family_index(&self) -> u32 {
        self.transfer_queue_family_index
    }

    /// Whether `VK_KHR_external_memory` and `VK_KHR_external_memory_fd` are
    /// enabled.
    pub fn supports_external_memory(&self) -> bool {
        self.supports_external_memory
    }

    /// Whether the device is a CPU-type software rasterizer.
    pub fn device_is_software(&self) -> bool {
        self.device_is_software
    }

    /// Whether frames can cross as DMA-BUF; never on a software device.
    pub fn supports_dma_buf_export(&self) -> bool {
        self.supports_dma_buf_export
    }

    /// Whether a timeline semaphore can be exported as an OPAQUE_FD.
    pub fn supports_semaphore_export(&self) -> bool {
        self.supports_semaphore_export
    }

    /// Whether the driver tolerates a failed cross-device DMA-BUF import
    /// attempt without perturbing per-handle-type kernel accounting for
    /// other export handle types (notably OPAQUE_FD).
    ///
    /// Cross-device meaning: the source DMA-BUF was allocated by a
    /// different DRM device (e.g. UVC camera kernel driver) than this
    /// `VkDevice`. Same-device DMA-BUF imports (e.g. a helper process's
    /// import of this device's own frames) are unaffected and always safe.
    ///
    /// Returns `false` on NVIDIA Linux today: failed cross-device DMA-BUF
    /// imports flake the engine's OPAQUE_FD allocation budget for the
    /// device's lifetime, even with the engine-side OPAQUE_FD sentinels
    /// from #637 in place. Returns `true` on Mesa drivers (Intel iris,
    /// AMD radeonsi) where the probe runs without observable side
    /// effects.
    pub fn supports_cross_device_dma_buf_probe(&self) -> bool {
        self.supports_cross_device_dma_buf_probe
    }

    /// Get the dedicated compute queue family index (if available).
    pub fn compute_queue_family_index(&self) -> Option<u32> {
        self.compute_queue_family_index
    }
}

impl HostVulkanDevice {
    /// Whether `VK_EXT_external_memory_host` was enabled at device
    /// construction — the imported tier of
    /// [`HostMappingReadByGpu`](crate::vulkan::rhi::HostMappingReadByGpu) is reachable only then.
    pub fn supports_host_pointer_import(&self) -> bool {
        self.has_external_memory_host
    }

    /// The alignment every imported host range must start and end on
    /// (`minImportedHostPointerAlignment`); zero when
    /// [`Self::supports_host_pointer_import`] is false.
    pub fn min_imported_host_pointer_alignment(&self) -> u64 {
        self.min_imported_host_pointer_alignment
    }

    /// Whether the `VK_KHR_ray_tracing_pipeline` extension chain
    /// (acceleration_structure + ray_tracing_pipeline +
    /// deferred_host_operations + pipeline_library) was enabled at
    /// device creation. Gates [`VulkanRayTracingKernel`] and
    /// [`VulkanAccelerationStructure`] construction.
    pub fn supports_ray_tracing_pipeline(&self) -> bool {
        self.has_ray_tracing_pipeline
    }

    /// `VkPhysicalDeviceRayTracingPipelinePropertiesKHR` snapshot
    /// queried at device construction. `Some(_)` whenever
    /// [`Self::supports_ray_tracing_pipeline`] is true.
    pub fn ray_tracing_pipeline_properties(&self) -> Option<RayTracingPipelineProperties> {
        self.ray_tracing_properties
    }

    /// Refuse a ray-tracing constructor on a device without the ray-tracing
    /// tier, naming the tier.
    pub(crate) fn refuse_without_the_ray_tracing_tier(
        &self,
        constructor_description: &str,
    ) -> crate::core::Result<()> {
        if self.has_ray_tracing_pipeline {
            return Ok(());
        }
        Err(ray_tracing_tier_absent_refusal(
            constructor_description,
            &self.device_name,
        ))
    }

    /// The subgroup operations and stages this device's driver serves,
    /// checked against each kernel stage before anything is built.
    pub(crate) fn subgroup_operation_support(&self) -> &VulkanSubgroupOperationSupport {
        &self.subgroup_operation_support
    }
}
