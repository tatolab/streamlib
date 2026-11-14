// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

impl GpuContext {
    /// Build a triangle-geometry bottom-level acceleration structure
    /// from CPU-side vertex + index data. Backs [`Self::create_ray_tracing_kernel`]
    /// — every TLAS instance references one of these BLAS handles.
    /// Returns a clean error when the device lacks RT support.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn build_triangles_blas(
        &self,
        label: &str,
        vertices: &[f32],
        indices: &[u32],
    ) -> Result<crate::vulkan::rhi::VulkanAccelerationStructure> {
        tracing::debug!(
            rhi_op = "build_triangles_blas",
            label,
            vertex_count = vertices.len() / 3,
            triangle_count = indices.len() / 3,
            "GpuContext::build_triangles_blas"
        );
        let vulkan_device = &self.device.inner;
        crate::vulkan::rhi::VulkanAccelerationStructure::build_triangles_blas(
            vulkan_device,
            label,
            vertices,
            indices,
        )
    }

    /// Build a top-level acceleration structure from a list of TLAS
    /// instances. Each instance references a BLAS the TLAS keeps alive
    /// for its lifetime. Returns a clean error when the device lacks
    /// RT support.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn build_tlas(
        &self,
        label: &str,
        instances: &[crate::vulkan::rhi::TlasInstanceDesc],
    ) -> Result<crate::vulkan::rhi::VulkanAccelerationStructure> {
        tracing::debug!(
            rhi_op = "build_tlas",
            label,
            instance_count = instances.len(),
            "GpuContext::build_tlas"
        );
        let vulkan_device = &self.device.inner;
        crate::vulkan::rhi::VulkanAccelerationStructure::build_tlas(vulkan_device, label, instances)
    }

    /// Whether the underlying GPU exposes the
    /// `VK_KHR_ray_tracing_pipeline` extension chain. RT-dependent
    /// consumers should check this before calling
    /// [`Self::create_ray_tracing_kernel`] /
    /// [`Self::build_triangles_blas`] / [`Self::build_tlas`].
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn supports_ray_tracing_pipeline(&self) -> bool {
        self.device.inner.supports_ray_tracing_pipeline()
    }

    /// Take ownership of a freshly built acceleration structure and return the
    /// id a later trace names it by.
    ///
    /// Every call mints a fresh id: an acceleration structure holds device
    /// memory proportional to its mesh, so unlike a kernel it is registered
    /// rather than deduplicated by content.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn register_acceleration_structure(
        &self,
        acceleration_structure: crate::vulkan::rhi::VulkanAccelerationStructure,
    ) -> String {
        let acceleration_structure_id = uuid::Uuid::new_v4().to_string();
        self.acceleration_structure_registry.lock().unwrap().insert(
            acceleration_structure_id.clone(),
            Arc::new(acceleration_structure),
        );
        acceleration_structure_id
    }

    /// Look up an acceleration structure a prior
    /// `register_acceleration_structure` returned the id of.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn acceleration_structure_by_id(
        &self,
        acceleration_structure_id: &str,
    ) -> Option<Arc<crate::vulkan::rhi::VulkanAccelerationStructure>> {
        self.acceleration_structure_registry
            .lock()
            .unwrap()
            .get(acceleration_structure_id)
            .map(Arc::clone)
    }

    /// Drop the registry's strong reference to an acceleration structure,
    /// answering whether an entry was there to drop.
    ///
    /// This is what a Rust caller's `VulkanAccelerationStructure` going out of
    /// scope does. The device memory returns once the last reference does — a
    /// TLAS holds its own reference to every BLAS it instances, so releasing a
    /// BLAS a scene still uses frees nothing until the scene goes too.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn release_acceleration_structure(&self, acceleration_structure_id: &str) -> bool {
        self.acceleration_structure_registry
            .lock()
            .unwrap()
            .remove(acceleration_structure_id)
            .is_some()
    }
}
