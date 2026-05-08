// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Vulkan acceleration-structure RHI primitive — bottom-level (BLAS) and
//! top-level (TLAS) `VkAccelerationStructureKHR` lifecycle.
//!
//! v1 shape: build-once / use / destroy. Compaction, refit, and rebuild are
//! deliberately out of scope and tracked as follow-ups; the simple shape is
//! what backs [`super::VulkanRayTracingKernel`]'s tests + the
//! `examples/raytracing-showcase` example, and grows into the richer
//! lifecycle when a consumer needs it.

#![cfg(target_os = "linux")]

use std::mem;
use std::sync::Arc;

use vulkanalia::prelude::v1_4::*;
use vulkanalia::vk;
use vulkanalia::vk::KhrAccelerationStructureExtensionDeviceCommands as _;
use vulkanalia_vma as vma;
use vma::Alloc as _;

use crate::core::{Result, StreamError};

use super::HostVulkanDevice;

/// Whether an acceleration structure stores geometry directly (BLAS) or
/// references other acceleration structures (TLAS).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccelerationStructureKind {
    BottomLevel,
    TopLevel,
}

/// Description of one TLAS instance: a transform, a 24-bit custom index
/// the hit shader can read via `gl_InstanceCustomIndexEXT`, a visibility
/// mask, an SBT record offset, and the BLAS this instance points to.
///
/// The BLAS reference is by `&Arc<VulkanAccelerationStructure>` so the
/// lifetime contract is "the TLAS holds a strong reference to every
/// referenced BLAS for as long as the TLAS lives."
#[derive(Clone)]
pub struct TlasInstanceDesc {
    /// Row-major 3×4 affine transform applied to the BLAS geometry in
    /// world space. Matches `VkTransformMatrixKHR` exactly.
    pub transform: [[f32; 4]; 3],
    /// 24-bit user data exposed to hit shaders as `gl_InstanceCustomIndexEXT`.
    pub custom_index: u32,
    /// 8-bit visibility mask. Rays specify a `cullMask`; the instance
    /// is hit only when `(mask & cullMask) != 0`.
    pub mask: u8,
    /// Offset added to the SBT hit-group index (kernel ABI: usually 0
    /// for single-hit-group pipelines).
    pub sbt_record_offset: u32,
    /// Vulkan instance flags (face culling, opacity overrides, etc.).
    /// Default: opaque + counterclockwise front face.
    pub flags: vk::GeometryInstanceFlagsKHR,
    /// BLAS this instance references. Kept alive by the TLAS via a clone.
    pub blas: Arc<VulkanAccelerationStructure>,
}

impl TlasInstanceDesc {
    /// Identity transform, opaque + counterclockwise front face. The
    /// most common TLAS-instance shape.
    pub fn identity(blas: Arc<VulkanAccelerationStructure>) -> Self {
        Self {
            transform: IDENTITY_TRANSFORM,
            custom_index: 0,
            mask: 0xff,
            sbt_record_offset: 0,
            flags: vk::GeometryInstanceFlagsKHR::TRIANGLE_FACING_CULL_DISABLE
                | vk::GeometryInstanceFlagsKHR::FORCE_OPAQUE,
            blas,
        }
    }
}

/// Row-major 3×4 identity transform.
pub const IDENTITY_TRANSFORM: [[f32; 4]; 3] = [
    [1.0, 0.0, 0.0, 0.0],
    [0.0, 1.0, 0.0, 0.0],
    [0.0, 0.0, 1.0, 0.0],
];

/// One built `VkAccelerationStructureKHR` (BLAS or TLAS) with its owning
/// storage buffer + allocation.
pub struct VulkanAccelerationStructure {
    label: String,
    kind: AccelerationStructureKind,
    vulkan_device: Arc<HostVulkanDevice>,
    handle: vk::AccelerationStructureKHR,
    /// Device address of the AS, queried via
    /// `vkGetAccelerationStructureDeviceAddressKHR`. Used as
    /// `accelerationStructureReference` when this AS appears as a BLAS in
    /// a TLAS instance.
    device_address: u64,
    storage_buffer: vk::Buffer,
    storage_allocation: Option<vma::Allocation>,
    storage_size: vk::DeviceSize,
    /// BLAS references this TLAS keeps alive. Empty for BLAS.
    referenced_blases: Vec<Arc<VulkanAccelerationStructure>>,
}

impl VulkanAccelerationStructure {
    /// Build a triangle-geometry bottom-level acceleration structure from
    /// CPU-side vertex + index data. Vertices are interleaved
    /// `[x, y, z, x, y, z, ...]` (R32G32B32_SFLOAT, stride 12 bytes);
    /// indices are 32-bit unsigned, three per triangle.
    ///
    /// Uploads the data via a transient HOST_VISIBLE staging buffer +
    /// `vkCmdCopyBuffer`, then submits a `vkCmdBuildAccelerationStructuresKHR`
    /// build and waits before returning. The staging + scratch buffers
    /// are freed on success.
    pub fn build_triangles_blas(
        vulkan_device: &Arc<HostVulkanDevice>,
        label: &str,
        vertices: &[f32],
        indices: &[u32],
    ) -> Result<Arc<Self>> {
        if !vulkan_device.supports_ray_tracing_pipeline() {
            return Err(StreamError::GpuError(format!(
                "Acceleration structure '{label}': ray-tracing extensions not supported by device"
            )));
        }
        if vertices.is_empty() || indices.is_empty() {
            return Err(StreamError::GpuError(format!(
                "Acceleration structure '{label}': empty geometry (vertices={}, indices={})",
                vertices.len(),
                indices.len()
            )));
        }
        if vertices.len() % 3 != 0 {
            return Err(StreamError::GpuError(format!(
                "Acceleration structure '{label}': vertex slice length {} is not a multiple of 3 (must be flat [x,y,z,...] layout)",
                vertices.len()
            )));
        }
        if indices.len() % 3 != 0 {
            return Err(StreamError::GpuError(format!(
                "Acceleration structure '{label}': index slice length {} is not a multiple of 3 (must be three indices per triangle)",
                indices.len()
            )));
        }

        let device = vulkan_device.device();
        let triangle_count = (indices.len() / 3) as u32;
        let vertex_count = (vertices.len() / 3) as u32;
        let vertex_bytes = mem::size_of_val(vertices) as vk::DeviceSize;
        let index_bytes = mem::size_of_val(indices) as vk::DeviceSize;

        // Vertex buffer — HOST_VISIBLE so we memcpy + skip the staging
        // copy submission. Removes the cross-submit memory-visibility
        // class of bug that bites silently (every ray missed in the
        // initial implementation because the AS build read pre-copy
        // garbage from the DEVICE_LOCAL inputs).
        let vertex_buffer = AsBuffer::new_host_visible(
            vulkan_device,
            vertex_bytes,
            vk::BufferUsageFlags::ACCELERATION_STRUCTURE_BUILD_INPUT_READ_ONLY_KHR
                | vk::BufferUsageFlags::SHADER_DEVICE_ADDRESS,
            &format!("{label}/vertex"),
        )?;
        let vptr = vertex_buffer.mapped_ptr();
        if vptr.is_null() {
            return Err(StreamError::GpuError(format!(
                "Acceleration structure '{label}': vertex buffer mapping returned null"
            )));
        }
        unsafe {
            std::ptr::copy_nonoverlapping(
                vertices.as_ptr() as *const u8,
                vptr,
                vertex_bytes as usize,
            );
        }

        // Index buffer — same shape.
        let index_buffer = AsBuffer::new_host_visible(
            vulkan_device,
            index_bytes,
            vk::BufferUsageFlags::ACCELERATION_STRUCTURE_BUILD_INPUT_READ_ONLY_KHR
                | vk::BufferUsageFlags::SHADER_DEVICE_ADDRESS,
            &format!("{label}/index"),
        )?;
        let iptr = index_buffer.mapped_ptr();
        if iptr.is_null() {
            return Err(StreamError::GpuError(format!(
                "Acceleration structure '{label}': index buffer mapping returned null"
            )));
        }
        unsafe {
            std::ptr::copy_nonoverlapping(
                indices.as_ptr() as *const u8,
                iptr,
                index_bytes as usize,
            );
        }

        // Geometry descriptor.
        let mut triangles_data =
            vk::AccelerationStructureGeometryTrianglesDataKHR::builder()
                .vertex_format(vk::Format::R32G32B32_SFLOAT)
                .vertex_data(vk::DeviceOrHostAddressConstKHR {
                    device_address: vertex_buffer.device_address,
                })
                .vertex_stride(12)
                .max_vertex(vertex_count.saturating_sub(1))
                .index_type(vk::IndexType::UINT32)
                .index_data(vk::DeviceOrHostAddressConstKHR {
                    device_address: index_buffer.device_address,
                })
                .transform_data(vk::DeviceOrHostAddressConstKHR { device_address: 0 })
                .build();

        let geometry = vk::AccelerationStructureGeometryKHR::builder()
            .geometry_type(vk::GeometryTypeKHR::TRIANGLES)
            .geometry(vk::AccelerationStructureGeometryDataKHR {
                triangles: triangles_data,
            })
            .flags(vk::GeometryFlagsKHR::OPAQUE)
            .build();
        // Suppress the unused-but-needed-for-lifetime warning on the
        // triangles builder — its data lives via the geometry union above.
        let _ = &mut triangles_data;

        let geometries = [geometry];
        let build_geometry_info =
            vk::AccelerationStructureBuildGeometryInfoKHR::builder()
                .type_(vk::AccelerationStructureTypeKHR::BOTTOM_LEVEL)
                .flags(vk::BuildAccelerationStructureFlagsKHR::PREFER_FAST_TRACE)
                .mode(vk::BuildAccelerationStructureModeKHR::BUILD)
                .geometries(&geometries)
                .build();

        let max_primitive_counts = [triangle_count];
        let mut size_info =
            vk::AccelerationStructureBuildSizesInfoKHR::builder().build();
        unsafe {
            device.get_acceleration_structure_build_sizes_khr(
                vk::AccelerationStructureBuildTypeKHR::DEVICE,
                &build_geometry_info,
                &max_primitive_counts,
                &mut size_info,
            );
        }

        Self::finish_build(
            vulkan_device,
            label,
            AccelerationStructureKind::BottomLevel,
            size_info,
            build_geometry_info,
            geometries,
            vk::AccelerationStructureBuildRangeInfoKHR::builder()
                .primitive_count(triangle_count)
                .primitive_offset(0)
                .first_vertex(0)
                .transform_offset(0)
                .build(),
            Vec::new(),
            // Drop these only after the build submit waits.
            vec![vertex_buffer, index_buffer],
        )
    }

    /// Build a top-level acceleration structure from a list of TLAS
    /// instances. Each instance references a `VulkanAccelerationStructure`
    /// of [`AccelerationStructureKind::BottomLevel`]; the TLAS keeps a
    /// strong reference to every BLAS for its lifetime.
    pub fn build_tlas(
        vulkan_device: &Arc<HostVulkanDevice>,
        label: &str,
        instances: &[TlasInstanceDesc],
    ) -> Result<Arc<Self>> {
        if !vulkan_device.supports_ray_tracing_pipeline() {
            return Err(StreamError::GpuError(format!(
                "Acceleration structure '{label}': ray-tracing extensions not supported by device"
            )));
        }
        if instances.is_empty() {
            return Err(StreamError::GpuError(format!(
                "Acceleration structure '{label}': TLAS must have at least one instance"
            )));
        }
        for (i, inst) in instances.iter().enumerate() {
            if inst.blas.kind != AccelerationStructureKind::BottomLevel {
                return Err(StreamError::GpuError(format!(
                    "Acceleration structure '{label}': instance {i} references a TLAS as its BLAS"
                )));
            }
        }

        let device = vulkan_device.device();
        let referenced_blases: Vec<Arc<VulkanAccelerationStructure>> =
            instances.iter().map(|i| Arc::clone(&i.blas)).collect();

        // Serialize each instance to its spec-defined 64-byte layout.
        // See `instance_bytes` doc comment for why we don't use
        // `vk::AccelerationStructureInstanceKHR` directly.
        let mut instance_blob = Vec::with_capacity(instances.len() * INSTANCE_BYTES);
        for inst in instances {
            instance_blob.extend_from_slice(&instance_bytes(inst));
        }
        let instance_total_bytes = instance_blob.len() as vk::DeviceSize;

        let instance_buffer = AsBuffer::new_host_visible(
            vulkan_device,
            instance_total_bytes,
            vk::BufferUsageFlags::ACCELERATION_STRUCTURE_BUILD_INPUT_READ_ONLY_KHR
                | vk::BufferUsageFlags::SHADER_DEVICE_ADDRESS,
            &format!("{label}/instances"),
        )?;
        let inst_ptr = instance_buffer.mapped_ptr();
        if inst_ptr.is_null() {
            return Err(StreamError::GpuError(format!(
                "Acceleration structure '{label}': instance buffer mapping returned null"
            )));
        }
        unsafe {
            std::ptr::copy_nonoverlapping(
                instance_blob.as_ptr(),
                inst_ptr,
                instance_blob.len(),
            );
        }

        let instances_data = vk::AccelerationStructureGeometryInstancesDataKHR::builder()
            .array_of_pointers(false)
            .data(vk::DeviceOrHostAddressConstKHR {
                device_address: instance_buffer.device_address,
            })
            .build();

        let geometry = vk::AccelerationStructureGeometryKHR::builder()
            .geometry_type(vk::GeometryTypeKHR::INSTANCES)
            .geometry(vk::AccelerationStructureGeometryDataKHR {
                instances: instances_data,
            })
            .flags(vk::GeometryFlagsKHR::OPAQUE)
            .build();

        let geometries = [geometry];
        let build_geometry_info =
            vk::AccelerationStructureBuildGeometryInfoKHR::builder()
                .type_(vk::AccelerationStructureTypeKHR::TOP_LEVEL)
                .flags(vk::BuildAccelerationStructureFlagsKHR::PREFER_FAST_TRACE)
                .mode(vk::BuildAccelerationStructureModeKHR::BUILD)
                .geometries(&geometries)
                .build();

        let max_primitive_counts = [instances.len() as u32];
        let mut size_info =
            vk::AccelerationStructureBuildSizesInfoKHR::builder().build();
        unsafe {
            device.get_acceleration_structure_build_sizes_khr(
                vk::AccelerationStructureBuildTypeKHR::DEVICE,
                &build_geometry_info,
                &max_primitive_counts,
                &mut size_info,
            );
        }

        Self::finish_build(
            vulkan_device,
            label,
            AccelerationStructureKind::TopLevel,
            size_info,
            build_geometry_info,
            geometries,
            vk::AccelerationStructureBuildRangeInfoKHR::builder()
                .primitive_count(instances.len() as u32)
                .primitive_offset(0)
                .first_vertex(0)
                .transform_offset(0)
                .build(),
            referenced_blases,
            vec![instance_buffer],
        )
    }

    fn finish_build(
        vulkan_device: &Arc<HostVulkanDevice>,
        label: &str,
        kind: AccelerationStructureKind,
        size_info: vk::AccelerationStructureBuildSizesInfoKHR,
        mut build_geometry_info: vk::AccelerationStructureBuildGeometryInfoKHR,
        geometries: [vk::AccelerationStructureGeometryKHR; 1],
        range_info: vk::AccelerationStructureBuildRangeInfoKHR,
        referenced_blases: Vec<Arc<VulkanAccelerationStructure>>,
        transient_inputs: Vec<AsBuffer>,
    ) -> Result<Arc<Self>> {
        let device = vulkan_device.device();

        // 1. AS storage buffer.
        let storage_size = size_info.acceleration_structure_size;
        let storage = AsBuffer::new(
            vulkan_device,
            storage_size,
            vk::BufferUsageFlags::ACCELERATION_STRUCTURE_STORAGE_KHR
                | vk::BufferUsageFlags::SHADER_DEVICE_ADDRESS,
            &format!("{label}/storage"),
        )?;

        // 2. Create the VkAccelerationStructureKHR on top of the storage.
        let as_type = match kind {
            AccelerationStructureKind::BottomLevel => {
                vk::AccelerationStructureTypeKHR::BOTTOM_LEVEL
            }
            AccelerationStructureKind::TopLevel => {
                vk::AccelerationStructureTypeKHR::TOP_LEVEL
            }
        };
        let as_create_info = vk::AccelerationStructureCreateInfoKHR::builder()
            .buffer(storage.buffer)
            .offset(0)
            .size(storage_size)
            .type_(as_type)
            .build();
        let handle = unsafe {
            device.create_acceleration_structure_khr(&as_create_info, None)
        }
        .map_err(|e| {
            StreamError::GpuError(format!(
                "Acceleration structure '{label}': vkCreateAccelerationStructureKHR failed: {e}"
            ))
        })?;

        // Plug the AS handle into the build info.
        build_geometry_info.dst_acceleration_structure = handle;
        build_geometry_info.geometry_count = geometries.len() as u32;
        build_geometry_info.geometries = geometries.as_ptr();

        // The remaining steps (scratch alloc + record + submit + wait)
        // each need to destroy `handle` if they fail — otherwise the
        // VkAccelerationStructureKHR leaks. Wrap them in an inner
        // closure that returns Result and apply the destroy on Err
        // exactly once at the call site, instead of duplicating the
        // cleanup at every `?` site.
        let build_result: Result<()> = (|| {
            // 3. Scratch buffer (the build needs this; freed once the build completes).
            let scratch = AsBuffer::new(
                vulkan_device,
                size_info.build_scratch_size,
                vk::BufferUsageFlags::STORAGE_BUFFER | vk::BufferUsageFlags::SHADER_DEVICE_ADDRESS,
                &format!("{label}/scratch"),
            )?;
            build_geometry_info.scratch_data = vk::DeviceOrHostAddressKHR {
                device_address: scratch.device_address,
            };

            // 4. Record + submit the build.
            let queue = vulkan_device.queue();
            let queue_family = vulkan_device.queue_family_index();
            let command_pool = create_one_shot_pool(device, queue_family, label)?;

            let cmd = match allocate_one_shot_cmd(device, command_pool) {
                Ok(c) => c,
                Err(e) => {
                    unsafe { device.destroy_command_pool(command_pool, None) };
                    drop(scratch);
                    return Err(e);
                }
            };

            let begin_info = vk::CommandBufferBeginInfo::builder()
                .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT)
                .build();
            if let Err(e) = unsafe { device.begin_command_buffer(cmd, &begin_info) } {
                unsafe { device.destroy_command_pool(command_pool, None) };
                drop(scratch);
                return Err(StreamError::GpuError(format!(
                    "Acceleration structure '{label}': begin_command_buffer failed: {e}"
                )));
            }

            // vulkanalia's `cmd_build_acceleration_structures_khr` wrapper has
            // a Rust→C ABI mismatch: it accepts `&[&[T]]` (slice of fat-pointer
            // slots, 16 bytes each on 64-bit) where the C signature is
            // `*const *const T` (array of thin pointers, 8 bytes each), and
            // casts the Rust slice pointer directly without rebuilding the
            // thin-pointer array. Workaround: build the thin-pointer array by
            // hand and call the function pointer directly.
            let range_infos = [range_info];
            let range_ptrs: [*const vk::AccelerationStructureBuildRangeInfoKHR; 1] =
                [range_infos.as_ptr()];
            let infos = [build_geometry_info];
            unsafe {
                (device.commands().cmd_build_acceleration_structures_khr)(
                    cmd,
                    infos.len() as u32,
                    infos.as_ptr(),
                    range_ptrs.as_ptr(),
                );
            }

            if let Err(e) = unsafe { device.end_command_buffer(cmd) } {
                unsafe { device.destroy_command_pool(command_pool, None) };
                drop(scratch);
                return Err(StreamError::GpuError(format!(
                    "Acceleration structure '{label}': end_command_buffer failed: {e}"
                )));
            }

            let fence_info = vk::FenceCreateInfo::builder().build();
            let fence = match unsafe { device.create_fence(&fence_info, None) } {
                Ok(f) => f,
                Err(e) => {
                    unsafe { device.destroy_command_pool(command_pool, None) };
                    drop(scratch);
                    return Err(StreamError::GpuError(format!(
                        "Acceleration structure '{label}': fence creation failed: {e}"
                    )));
                }
            };

            let cmd_info = vk::CommandBufferSubmitInfo::builder()
                .command_buffer(cmd)
                .build();
            let cmd_infos = [cmd_info];
            let submit = vk::SubmitInfo2::builder()
                .command_buffer_infos(&cmd_infos)
                .build();

            let submit_then_wait: Result<()> = unsafe {
                HostVulkanDevice::submit_to_queue(vulkan_device, queue, &[submit], fence)
            }
            .and_then(|_| {
                unsafe { device.wait_for_fences(&[fence], true, u64::MAX) }
                    .map(|_| ())
                    .map_err(|e| {
                        StreamError::GpuError(format!(
                            "Acceleration structure '{label}': wait_for_fences failed: {e}"
                        ))
                    })
            });

            unsafe {
                device.destroy_fence(fence, None);
                device.destroy_command_pool(command_pool, None);
            }
            drop(scratch);
            submit_then_wait
        })();

        if let Err(e) = build_result {
            unsafe { device.destroy_acceleration_structure_khr(handle, None) };
            drop(storage);
            drop(transient_inputs);
            return Err(e);
        }

        // 5. Query the AS device address (used as `accelerationStructureReference`
        // when this AS is referenced from a TLAS instance).
        let address_info = vk::AccelerationStructureDeviceAddressInfoKHR::builder()
            .acceleration_structure(handle)
            .build();
        let device_address = unsafe {
            device.get_acceleration_structure_device_address_khr(&address_info)
        };

        // Drop transient inputs now — the AS is built; scratch was freed
        // inside the build closure above.
        drop(transient_inputs);

        let (storage_buffer, storage_allocation, _storage_address) = storage.into_parts();

        Ok(Arc::new(Self {
            label: label.to_string(),
            kind,
            vulkan_device: Arc::clone(vulkan_device),
            handle,
            device_address,
            storage_buffer,
            storage_allocation: Some(storage_allocation),
            storage_size,
            referenced_blases,
        }))
    }

    /// `VkAccelerationStructureKHR` handle. Used for descriptor writes and
    /// for queries; never destroy this directly.
    pub fn vk_handle(&self) -> vk::AccelerationStructureKHR {
        self.handle
    }

    /// Device address of the AS. For a BLAS, this is the value placed in
    /// `VkAccelerationStructureInstanceKHR::accelerationStructureReference`
    /// when wiring it into a TLAS.
    pub fn device_address(&self) -> u64 {
        self.device_address
    }

    /// `BottomLevel` or `TopLevel`.
    pub fn kind(&self) -> AccelerationStructureKind {
        self.kind
    }

    /// Human-readable label used in diagnostics.
    pub fn label(&self) -> &str {
        &self.label
    }

    /// Storage size in bytes (the size returned by
    /// `vkGetAccelerationStructureBuildSizesKHR` at build time).
    pub fn storage_size(&self) -> vk::DeviceSize {
        self.storage_size
    }
}

impl Drop for VulkanAccelerationStructure {
    fn drop(&mut self) {
        unsafe {
            let device = self.vulkan_device.device();
            let _ = device.device_wait_idle();
            device.destroy_acceleration_structure_khr(self.handle, None);
            if let Some(allocation) = self.storage_allocation.take() {
                self.vulkan_device
                    .allocator()
                    .destroy_buffer(self.storage_buffer, allocation);
            }
        }
    }
}

unsafe impl Send for VulkanAccelerationStructure {}
unsafe impl Sync for VulkanAccelerationStructure {}

impl std::fmt::Debug for VulkanAccelerationStructure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VulkanAccelerationStructure")
            .field("label", &self.label)
            .field("kind", &self.kind)
            .field("device_address", &format_args!("{:#x}", self.device_address))
            .field("storage_size", &self.storage_size)
            .field("instances", &self.referenced_blases.len())
            .finish()
    }
}

// ---- Internal buffer helper -------------------------------------------------

/// Owning DEVICE_LOCAL buffer with a pre-queried device address. Internal
/// to the AS module — the engine's public buffer types
/// (`HostVulkanPixelBuffer`) target HOST_VISIBLE / OPAQUE_FD-export use cases
/// and don't carry a `BUFFER_DEVICE_ADDRESS` flag, which AS builds need.
struct AsBuffer {
    vulkan_device: Arc<HostVulkanDevice>,
    buffer: vk::Buffer,
    allocation: Option<vma::Allocation>,
    device_address: u64,
}

impl AsBuffer {
    fn new(
        vulkan_device: &Arc<HostVulkanDevice>,
        size: vk::DeviceSize,
        usage: vk::BufferUsageFlags,
        label: &str,
    ) -> Result<Self> {
        debug_assert!(size > 0, "AsBuffer::new called with zero size for '{label}'");
        let allocator = vulkan_device.allocator();
        let buffer_info = vk::BufferCreateInfo::builder()
            .size(size)
            .usage(usage)
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .build();
        let alloc_opts = vma::AllocationOptions {
            usage: vma::MemoryUsage::AutoPreferDevice,
            required_flags: vk::MemoryPropertyFlags::DEVICE_LOCAL,
            ..Default::default()
        };
        let (buffer, allocation) =
            unsafe { allocator.create_buffer(buffer_info, &alloc_opts) }.map_err(|e| {
                StreamError::GpuError(format!(
                    "AS buffer '{label}': vmaCreateBuffer (size={size}) failed: {e}"
                ))
            })?;

        let device_address = if usage.contains(vk::BufferUsageFlags::SHADER_DEVICE_ADDRESS) {
            let info = vk::BufferDeviceAddressInfo::builder().buffer(buffer).build();
            unsafe { vulkan_device.device().get_buffer_device_address(&info) }
        } else {
            0
        };

        Ok(Self {
            vulkan_device: Arc::clone(vulkan_device),
            buffer,
            allocation: Some(allocation),
            device_address,
        })
    }

    /// HOST_VISIBLE + HOST_COHERENT + MAPPED variant — used for AS build
    /// inputs (vertex / index / instance buffers) so the data can be
    /// memcpy'd directly to GPU-visible memory without a staging copy.
    /// Avoids the cross-submit memory-visibility class of bug that an
    /// upload-then-build pattern with separate `vkQueueSubmit` calls is
    /// vulnerable to. NVIDIA / AMD / Intel all expose enough HOST_VISIBLE
    /// memory types that this works for AS-build inputs in practice.
    fn new_host_visible(
        vulkan_device: &Arc<HostVulkanDevice>,
        size: vk::DeviceSize,
        usage: vk::BufferUsageFlags,
        label: &str,
    ) -> Result<Self> {
        debug_assert!(
            size > 0,
            "AsBuffer::new_host_visible called with zero size for '{label}'"
        );
        let allocator = vulkan_device.allocator();
        let buffer_info = vk::BufferCreateInfo::builder()
            .size(size)
            .usage(usage)
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .build();
        let alloc_opts = vma::AllocationOptions {
            usage: vma::MemoryUsage::AutoPreferHost,
            required_flags: vk::MemoryPropertyFlags::HOST_VISIBLE
                | vk::MemoryPropertyFlags::HOST_COHERENT,
            flags: vma::AllocationCreateFlags::MAPPED
                | vma::AllocationCreateFlags::HOST_ACCESS_SEQUENTIAL_WRITE,
            ..Default::default()
        };
        let (buffer, allocation) =
            unsafe { allocator.create_buffer(buffer_info, &alloc_opts) }.map_err(|e| {
                StreamError::GpuError(format!(
                    "AS buffer (host-visible) '{label}': vmaCreateBuffer (size={size}) failed: {e}"
                ))
            })?;

        let device_address = if usage.contains(vk::BufferUsageFlags::SHADER_DEVICE_ADDRESS) {
            let info = vk::BufferDeviceAddressInfo::builder().buffer(buffer).build();
            unsafe { vulkan_device.device().get_buffer_device_address(&info) }
        } else {
            0
        };

        Ok(Self {
            vulkan_device: Arc::clone(vulkan_device),
            buffer,
            allocation: Some(allocation),
            device_address,
        })
    }

    /// `pMappedData` pointer for a HOST_VISIBLE / MAPPED-flag allocation,
    /// or null if the allocation isn't host-mapped.
    fn mapped_ptr(&self) -> *mut u8 {
        if let Some(allocation) = self.allocation {
            let info = self.vulkan_device.allocator().get_allocation_info(allocation);
            info.pMappedData as *mut u8
        } else {
            std::ptr::null_mut()
        }
    }

    /// Consume the AsBuffer and return its parts without freeing — the
    /// caller takes ownership of the `vk::Buffer` + `vma::Allocation` and
    /// is responsible for `vmaDestroyBuffer` on drop.
    fn into_parts(mut self) -> (vk::Buffer, vma::Allocation, u64) {
        let buffer = self.buffer;
        let device_address = self.device_address;
        let allocation = self
            .allocation
            .take()
            .expect("AsBuffer always carries an allocation until consumed");
        // Skip Drop's free path — caller now owns the buffer + allocation.
        std::mem::forget(self);
        (buffer, allocation, device_address)
    }
}

impl Drop for AsBuffer {
    fn drop(&mut self) {
        if let Some(allocation) = self.allocation.take() {
            unsafe {
                self.vulkan_device
                    .allocator()
                    .destroy_buffer(self.buffer, allocation);
            }
        }
    }
}

fn create_one_shot_pool(
    device: &vulkanalia::Device,
    queue_family: u32,
    label: &str,
) -> Result<vk::CommandPool> {
    let info = vk::CommandPoolCreateInfo::builder()
        .queue_family_index(queue_family)
        .flags(vk::CommandPoolCreateFlags::TRANSIENT)
        .build();
    unsafe { device.create_command_pool(&info, None) }.map_err(|e| {
        StreamError::GpuError(format!(
            "AS one-shot pool '{label}': create_command_pool failed: {e}"
        ))
    })
}

fn allocate_one_shot_cmd(
    device: &vulkanalia::Device,
    pool: vk::CommandPool,
) -> Result<vk::CommandBuffer> {
    let info = vk::CommandBufferAllocateInfo::builder()
        .command_pool(pool)
        .level(vk::CommandBufferLevel::PRIMARY)
        .command_buffer_count(1)
        .build();
    let buffers = unsafe { device.allocate_command_buffers(&info) }.map_err(|e| {
        StreamError::GpuError(format!(
            "AS one-shot cmd: allocate_command_buffers failed: {e}"
        ))
    })?;
    Ok(buffers[0])
}

/// Layout of `VkAccelerationStructureInstanceKHR` per the Vulkan spec
/// (`transform`, packed bitfields, `accelerationStructureReference`),
/// serialized into a flat `[u8; 64]`.
///
/// Working around a layout bug in `vulkanalia-sys` 0.35.0:
/// `vk::AccelerationStructureInstanceKHR` orders the fields
/// `transform`, `acceleration_structure_reference`, `bitfields0`,
/// `bitfields1` — putting `accel_ref` BEFORE the two bitfields. The
/// Vulkan C spec orders them the other way (transform, bitfields,
/// accel_ref). Because the struct is `#[repr(C)]` the GPU reads each
/// field at its spec-defined offset, so vulkanalia's struct writes
/// `accel_ref` at offset 48 and the bitfields at offsets 56/60 — but
/// the GPU reads bitfields at 48/52 and `accel_ref` at 56. The result
/// is a TLAS whose instances point at garbage BLAS addresses; every
/// ray misses, every frame is just the miss-shader output. Writing
/// the bytes manually in spec order sidesteps the bug entirely.
const INSTANCE_BYTES: usize = 64;

fn instance_bytes(desc: &TlasInstanceDesc) -> [u8; INSTANCE_BYTES] {
    let mut out = [0u8; INSTANCE_BYTES];

    // bytes 0..48 — transform: row-major 3×4 floats.
    let mut off = 0;
    for row in 0..3 {
        for col in 0..4 {
            out[off..off + 4].copy_from_slice(&desc.transform[row][col].to_ne_bytes());
            off += 4;
        }
    }
    debug_assert_eq!(off, 48);

    // bytes 48..52 — instanceCustomIndex (24) + mask (8) packed u32.
    let custom_index = desc.custom_index & 0x00ff_ffff;
    let mask = (desc.mask as u32) << 24;
    out[48..52].copy_from_slice(&(custom_index | mask).to_ne_bytes());

    // bytes 52..56 — instanceShaderBindingTableRecordOffset (24) + flags (8) packed u32.
    let sbt = desc.sbt_record_offset & 0x00ff_ffff;
    let flags = (desc.flags.bits() & 0xff) << 24;
    out[52..56].copy_from_slice(&(sbt | flags).to_ne_bytes());

    // bytes 56..64 — accelerationStructureReference (BLAS device address).
    out[56..64].copy_from_slice(&desc.blas.device_address.to_ne_bytes());

    out
}

