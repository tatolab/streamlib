// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::ffi::c_char;
use std::sync::atomic::AtomicUsize;
use std::sync::{Arc, Mutex};

use vulkanalia::prelude::v1_4::*;
use vulkanalia::vk;
use vulkanalia_vma as vma;

use crate::core::{Error, Result};
use crate::vulkan::rhi::command_recording_and_sync::vulkan_queue_submission_completion::HostVulkanQueueSubmissionCompletionTimelines;
#[cfg(target_os = "linux")]
use crate::vulkan::rhi::device::drm_modifier_probe;
#[cfg(target_os = "linux")]
use crate::vulkan::rhi::device::drm_modifier_probe::DrmModifierTable;
use crate::vulkan::rhi::device::vulkan_device_selection::{
    VulkanDevicePassOverReason, VulkanPhysicalDeviceCandidate,
    build_on_the_first_candidate_that_accepts,
};
use crate::vulkan::rhi::device::vulkan_instance::HostVulkanInstance;
use crate::vulkan::rhi::kernels::vulkan_kernel_capability_refusal::VulkanSubgroupOperationSupport;
use crate::vulkan::rhi::memory_and_buffers::vulkan_frame_allocation_device_memory_budget::FrameAllocationDeviceMemoryBudgetGate;
#[cfg(target_os = "macos")]
use crate::vulkan::rhi::memory_and_buffers::vulkan_metal_working_set_budget::{
    MetalDeviceWhoseWorkingSetBoundsFrameAllocations, MetalWorkingSetOfTheDevice,
};
use streamlib_consumer_rhi::vulkan_extension_names_borrowed_from_properties;

#[cfg(target_os = "linux")]
use super::drm_and_opaque_fd_physical_device_probes::{
    drm_render_node_number_of_the_physical_device,
    physical_device_exports_an_opaque_fd_timeline_semaphore,
};
use super::{
    HostVulkanDevice, HostVulkanQueueFamilySelection, RayTracingPipelineProperties,
    VulkanLogicalDeviceDestroyedUnlessTaken,
};

impl HostVulkanDevice {
    pub fn new() -> Result<Arc<Self>> {
        let vulkan_instance = Arc::new(HostVulkanInstance::new()?);

        let physical_devices = unsafe { vulkan_instance.instance.enumerate_physical_devices() }
            .map_err(|e| Error::GpuError(format!("Failed to enumerate devices: {e}")))?;

        if physical_devices.is_empty() {
            return Err(Error::GpuError(
                "A Vulkan loader is present but reports no GPU. Check that your GPU vendor's \
                 Vulkan driver is installed and `vulkaninfo` lists a device."
                    .into(),
            ));
        }

        let candidates = physical_devices
            .into_iter()
            .map(|physical_device| {
                let properties = unsafe {
                    vulkan_instance
                        .instance
                        .get_physical_device_properties(physical_device)
                };
                VulkanPhysicalDeviceCandidate {
                    physical_device,
                    device_name: properties
                        .device_name
                        .as_cstr()
                        .to_string_lossy()
                        .into_owned(),
                    device_type: properties.device_type,
                    device_properties: properties,
                }
            })
            .collect();

        let selection = build_on_the_first_candidate_that_accepts(candidates, |candidate| {
            Self::build_on_physical_device(&vulkan_instance, candidate)
        })?;
        Ok(selection.log_the_chosen_device_beside_every_pass_over())
    }

    /// Build the logical device, its queues, allocator and export pools on
    /// one candidate. A refusal drops only what this candidate created; the
    /// instance stays for the next.
    fn build_on_physical_device(
        vulkan_instance: &Arc<HostVulkanInstance>,
        candidate: &VulkanPhysicalDeviceCandidate,
    ) -> std::result::Result<Arc<Self>, VulkanDevicePassOverReason> {
        let instance = &vulkan_instance.instance;
        let physical_device = candidate.physical_device;
        let device_is_software = candidate.device_type == vk::PhysicalDeviceType::CPU;

        let device_props = candidate.device_properties;
        let device_name = candidate.device_name.clone();
        let min_storage_buffer_offset_alignment =
            device_props.limits.min_storage_buffer_offset_alignment;
        let max_memory_allocation_count = device_props.limits.max_memory_allocation_count;

        // PCI vendor IDs assigned by Khronos Vulkan registry. NVIDIA's
        // proprietary Linux driver (and currently NVK on the same
        // hardware) report 0x10DE; failed cross-device DMA-BUF imports
        // perturb the engine's OPAQUE_FD allocation accounting on this
        // driver family per issue #638, so consumers must skip the probe.
        // Mesa drivers (iris, radeonsi) tolerate the failed import; the
        // probe runs as before. If a future NVIDIA driver release fixes
        // this, the blocklist is a one-line update.
        const PCI_VENDOR_NVIDIA: u32 = 0x10DE;
        let supports_cross_device_dma_buf_probe = device_props.vendor_id != PCI_VENDOR_NVIDIA;

        // 5b. Query VkPhysicalDeviceIDProperties::deviceUUID via
        //     vkGetPhysicalDeviceProperties2. CUDA-Vulkan interop matches
        //     this against `cudaDeviceProp::uuid` (16 bytes) to pick the
        //     right CUDA device; on multi-GPU rigs (Jetson + dGPU,
        //     prosumer workstations) using a mismatched device fails
        //     silently when CUDA imports the OPAQUE_FD memory.
        let mut id_props = vk::PhysicalDeviceIDProperties::default();
        let mut subgroup_props = vk::PhysicalDeviceSubgroupProperties::default();
        let mut driver_props = vk::PhysicalDeviceDriverProperties::default();
        let mut props2 = vk::PhysicalDeviceProperties2::builder()
            .push_next(&mut id_props)
            .push_next(&mut subgroup_props)
            .push_next(&mut driver_props)
            .build();
        unsafe { instance.get_physical_device_properties2(physical_device, &mut props2) };
        let subgroup_operation_support = VulkanSubgroupOperationSupport {
            driver_name: driver_props
                .driver_name
                .as_cstr()
                .to_string_lossy()
                .into_owned(),
            supported_stages: subgroup_props.supported_stages,
            supported_operations: subgroup_props.supported_operations,
        };
        tracing::info!(
            driver = %subgroup_operation_support.driver_name,
            supported_stages = ?subgroup_operation_support.supported_stages,
            supported_operations = ?subgroup_operation_support.supported_operations,
            "Vulkan subgroup operation support"
        );
        // `id_props.device_uuid` is `ByteArray<16>` (a transparent newtype
        // around `[u8; 16]`); use the upstream `From<ByteArray<N>> for [u8; N]`
        // impl to land in plain-array shape.
        let physical_device_uuid: [u8; 16] = id_props.device_uuid.into();

        // 6. The queue families the engine device is created with.
        let queue_families =
            unsafe { instance.get_physical_device_queue_family_properties(physical_device) };
        let HostVulkanQueueFamilySelection {
            graphics_queue_family_index: queue_family_index,
            transfer_queue_family_index,
            compute_queue_family_index,
        } = HostVulkanQueueFamilySelection::from_queue_families(&queue_families)?;
        if transfer_queue_family_index != queue_family_index {
            tracing::info!(
                "Dedicated transfer queue family found: {} (graphics: {})",
                transfer_queue_family_index,
                queue_family_index
            );
        } else {
            tracing::info!("No dedicated transfer queue — using graphics queue for transfers");
        }
        if let Some(cq_family) = compute_queue_family_index {
            tracing::info!("Dedicated compute queue family found: {}", cq_family);
        } else {
            tracing::info!("No dedicated compute queue family — using graphics queue for compute");
        }

        // 7. Create logical device with required extensions
        let queue_priorities = [1.0f32];
        let queue_create_infos: Vec<vk::DeviceQueueCreateInfo> = HostVulkanQueueFamilySelection {
            graphics_queue_family_index: queue_family_index,
            transfer_queue_family_index,
            compute_queue_family_index,
        }
        .distinct_queue_family_indices()
        .into_iter()
        .map(|family_index| {
            vk::DeviceQueueCreateInfo::builder()
                .queue_family_index(family_index)
                .queue_priorities(&queue_priorities)
                .build()
        })
        .collect();

        // Device extensions
        let mut device_extensions: Vec<*const c_char> = Vec::new();

        let available_device_extension_properties =
            unsafe { instance.enumerate_device_extension_properties(physical_device, None) }
                .inspect_err(|e| {
                    tracing::warn!(
                        error = %e,
                        "vkEnumerateDeviceExtensionProperties failed; treating this device as \
                         exposing no optional extensions"
                    );
                })
                .unwrap_or_default();
        let available_device_ext_names =
            vulkan_extension_names_borrowed_from_properties(&available_device_extension_properties);

        let has_memory_budget = {
            let memory_budget_ext = c"VK_EXT_memory_budget";
            let advertised = available_device_ext_names.contains(&memory_budget_ext);
            if advertised {
                device_extensions.push(memory_budget_ext.as_ptr());
                tracing::info!("VK_EXT_memory_budget enabled");
            } else if cfg!(target_os = "macos") {
                tracing::info!(
                    "VK_EXT_memory_budget is not advertised; frame allocations are bounded by \
                     Metal's recommended working set, which the extension does not change"
                );
            } else {
                tracing::warn!(
                    "VK_EXT_memory_budget is not advertised, so the bound on frame allocations \
                     is an estimate: 80% of each heap, a heap that is not device-local also \
                     capped by the system's available memory, weighed against this runtime's \
                     own allocator usage rather than the driver's"
                );
            }
            advertised
        };

        #[cfg(any(target_os = "macos", target_os = "ios"))]
        let has_external_memory_host = {
            // A portability implementation that advertises the subset extension
            // requires it to be enabled (VUID-VkDeviceCreateInfo-pProperties-04451),
            // so this is probed rather than pushed blind. The subset MoltenVK
            // withholds is pointPolygons, samplerMipLodBias, tessellationIsolines
            // and tessellationPointMode; the engine uses no geometry or
            // tessellation shaders, so none of it is reachable from here.
            let portability_subset_ext = c"VK_KHR_portability_subset";
            if available_device_ext_names.contains(&portability_subset_ext) {
                device_extensions.push(portability_subset_ext.as_ptr());
                tracing::info!("VK_KHR_portability_subset enabled");
            }

            // Metal interop — importing an IOSurface-backed CVPixelBuffer as a
            // VkImage goes through this. Device-level, unlike the surface
            // extension requested on the instance.
            let metal_objects_ext = c"VK_EXT_metal_objects";
            if available_device_ext_names.contains(&metal_objects_ext) {
                device_extensions.push(metal_objects_ext.as_ptr());
                tracing::info!("VK_EXT_metal_objects enabled - Metal interop available");
            } else {
                tracing::warn!(
                    "VK_EXT_metal_objects not available - Metal interop will be limited"
                );
            }

            // Import an IOSurface's pages as a storage buffer, zero-copy.
            // MoltenVK 1.4.0 advertises it but refuses the spec's
            // `VkExternalMemoryBufferCreateInfo{HOST_ALLOCATION}`; 1.4.1
            // accepts it, so the refusal surfaces at import time.
            let external_memory_host_ext = c"VK_EXT_external_memory_host";
            let has_external_memory_host =
                available_device_ext_names.contains(&external_memory_host_ext);
            if has_external_memory_host {
                device_extensions.push(external_memory_host_ext.as_ptr());
                tracing::info!("VK_EXT_external_memory_host enabled");
            }
            has_external_memory_host
        };

        // On Linux, check for DMA-BUF external memory extensions
        #[cfg(target_os = "linux")]
        let mut has_acquire_unmodified = false;
        #[cfg(target_os = "linux")]
        let mut has_external_memory_host = false;
        #[cfg(target_os = "linux")]
        let mut has_external_memory_dma_buf = false;
        #[cfg(target_os = "linux")]
        let mut has_image_drm_format_modifier = false;
        #[cfg(target_os = "linux")]
        let mut has_external_semaphore_fd = false;
        #[cfg(target_os = "linux")]
        let has_external_memory = {
            let external_memory_ext = c"VK_KHR_external_memory";
            let external_memory_fd_ext = c"VK_KHR_external_memory_fd";
            let external_memory_dmabuf_ext = c"VK_EXT_external_memory_dma_buf";

            let has_external_memory = available_device_ext_names.contains(&external_memory_ext)
                && available_device_ext_names.contains(&external_memory_fd_ext);

            if has_external_memory {
                device_extensions.push(external_memory_ext.as_ptr());
                device_extensions.push(external_memory_fd_ext.as_ptr());

                if available_device_ext_names.contains(&external_memory_dmabuf_ext) {
                    device_extensions.push(external_memory_dmabuf_ext.as_ptr());
                    has_external_memory_dma_buf = true;
                    tracing::info!("VK_EXT_external_memory_dma_buf available");
                }

                let drm_format_modifier_ext = c"VK_EXT_image_drm_format_modifier";
                if available_device_ext_names.contains(&drm_format_modifier_ext) {
                    device_extensions.push(drm_format_modifier_ext.as_ptr());
                    has_image_drm_format_modifier = true;
                    tracing::info!("VK_EXT_image_drm_format_modifier available");
                }

                // Required for cross-process timeline-semaphore handoff via
                // sync-fd: the host exports with vkGetSemaphoreFdKHR and the
                // subprocess imports with vkImportSemaphoreFdKHR. Without
                // this extension, surface-adapter sync across the IPC seam
                // cannot land. VK_KHR_external_semaphore is core since 1.1
                // so only the fd-handle subset needs explicit opt-in.
                let external_semaphore_fd_ext = c"VK_KHR_external_semaphore_fd";
                if available_device_ext_names.contains(&external_semaphore_fd_ext) {
                    device_extensions.push(external_semaphore_fd_ext.as_ptr());
                    has_external_semaphore_fd = true;
                    tracing::info!("VK_KHR_external_semaphore_fd available");
                }

                // Optional extension for spec-correct cross-process
                // layout coordination (#633).
                // `VK_EXT_external_memory_acquire_unmodified` lets the
                // consumer's acquire barrier chain
                // `VkExternalMemoryAcquireUnmodifiedEXT` so producer-
                // side content survives the QFOT acquire. When absent,
                // `acquire_from_foreign` falls back to bridging
                // UNDEFINED → target (content discard permitted by
                // spec but preserved in practice on NVIDIA Linux
                // DMA-BUF — and structurally permanent on NVIDIA per
                // current driver roadmap). `VK_QUEUE_FAMILY_EXTERNAL`
                // (used for QFOT src/dst) is core Vulkan 1.1 and
                // doesn't need an optional probe.
                let acquire_unmodified_ext = c"VK_EXT_external_memory_acquire_unmodified";
                if available_device_ext_names.contains(&acquire_unmodified_ext) {
                    device_extensions.push(acquire_unmodified_ext.as_ptr());
                    has_acquire_unmodified = true;
                    tracing::info!("VK_EXT_external_memory_acquire_unmodified available");
                }

                // Optional: import a host-owned mapping as device memory, so a
                // compute pass can write another driver's buffer in place.
                let external_memory_host_ext = c"VK_EXT_external_memory_host";
                if available_device_ext_names.contains(&external_memory_host_ext) {
                    device_extensions.push(external_memory_host_ext.as_ptr());
                    has_external_memory_host = true;
                    tracing::info!("VK_EXT_external_memory_host available");
                }

                tracing::info!("Vulkan external memory extensions enabled");
            } else {
                tracing::info!("Vulkan external memory extensions not available");
            }

            has_external_memory
        };

        // Ray-tracing pipeline extension chain — gates
        // `VulkanRayTracingKernel` and `VulkanAccelerationStructure`.
        // All four extensions must be present together; partial
        // support is not a thing the engine exposes. The feature
        // structs are pushed into the device-create chain only when
        // every extension is available so devices without RT do not
        // fail at create time on a missing-feature error.
        #[cfg(target_os = "linux")]
        let has_ray_tracing_pipeline = {
            let accel_struct_ext = c"VK_KHR_acceleration_structure";
            let rt_pipeline_ext = c"VK_KHR_ray_tracing_pipeline";
            let deferred_host_ext = c"VK_KHR_deferred_host_operations";
            let pipeline_library_ext = c"VK_KHR_pipeline_library";
            let all_present = available_device_ext_names.contains(&accel_struct_ext)
                && available_device_ext_names.contains(&rt_pipeline_ext)
                && available_device_ext_names.contains(&deferred_host_ext)
                && available_device_ext_names.contains(&pipeline_library_ext);
            if all_present {
                device_extensions.push(accel_struct_ext.as_ptr());
                device_extensions.push(rt_pipeline_ext.as_ptr());
                device_extensions.push(deferred_host_ext.as_ptr());
                device_extensions.push(pipeline_library_ext.as_ptr());
                tracing::info!(
                    "Vulkan ray-tracing extensions enabled \
                     (acceleration_structure + ray_tracing_pipeline + \
                      deferred_host_operations + pipeline_library)"
                );
            } else {
                tracing::info!(
                    "Vulkan ray-tracing extensions not available \
                     (accel_struct={}, rt_pipeline={}, deferred_host={}, \
                      pipeline_library={})",
                    available_device_ext_names.contains(&accel_struct_ext),
                    available_device_ext_names.contains(&rt_pipeline_ext),
                    available_device_ext_names.contains(&deferred_host_ext),
                    available_device_ext_names.contains(&pipeline_library_ext),
                );
            }
            all_present
        };
        #[cfg(not(target_os = "linux"))]
        let has_ray_tracing_pipeline = false;

        // VK_KHR_dynamic_rendering is core since Vulkan 1.3 — no extension string needed.
        // Feature struct (PhysicalDeviceDynamicRenderingFeatures) is still enabled below.

        // VK_KHR_synchronization2 is core since Vulkan 1.3 — no extension string needed.
        // Feature struct (PhysicalDeviceSynchronization2Features) is still enabled below.

        #[cfg(target_os = "linux")]
        let supports_external_memory = has_external_memory;
        #[cfg(not(target_os = "linux"))]
        let supports_external_memory = false;

        // DMA-BUF on a software device exists only where the process may open
        // `/dev/udmabuf`, a container's permissions; it is absent by rule.
        #[cfg(target_os = "linux")]
        let supports_dma_buf_export =
            has_external_memory_dma_buf && has_image_drm_format_modifier && !device_is_software;
        #[cfg(not(target_os = "linux"))]
        let supports_dma_buf_export = false;

        #[cfg(target_os = "linux")]
        let supports_semaphore_export = has_external_semaphore_fd
            && physical_device_exports_an_opaque_fd_timeline_semaphore(instance, physical_device);
        #[cfg(not(target_os = "linux"))]
        let supports_semaphore_export = false;

        #[cfg(not(target_os = "linux"))]
        let has_acquire_unmodified = false;
        #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "ios")))]
        let has_external_memory_host = false;
        #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "ios")))]
        let min_imported_host_pointer_alignment: u64 = 0;

        // Snapshot the RT pipeline properties (SBT alignment + handle
        // size + max recursion depth) before device creation. The query
        // is on the physical device; chained `VkPhysicalDeviceRayTracingPipelinePropertiesKHR`
        // is well-defined once the implementation advertises the extension.
        #[cfg(target_os = "linux")]
        let ray_tracing_properties = if has_ray_tracing_pipeline {
            let mut rt_props = vk::PhysicalDeviceRayTracingPipelinePropertiesKHR::builder().build();
            let mut props2 = vk::PhysicalDeviceProperties2::builder()
                .push_next(&mut rt_props)
                .build();
            unsafe { instance.get_physical_device_properties2(physical_device, &mut props2) };
            tracing::info!(
                "Ray-tracing pipeline properties: handle_size={}, handle_alignment={}, base_alignment={}, max_recursion_depth={}",
                rt_props.shader_group_handle_size,
                rt_props.shader_group_handle_alignment,
                rt_props.shader_group_base_alignment,
                rt_props.max_ray_recursion_depth,
            );
            Some(RayTracingPipelineProperties {
                shader_group_handle_size: rt_props.shader_group_handle_size,
                shader_group_handle_alignment: rt_props.shader_group_handle_alignment,
                shader_group_base_alignment: rt_props.shader_group_base_alignment,
                max_ray_recursion_depth: rt_props.max_ray_recursion_depth,
            })
        } else {
            None
        };
        #[cfg(not(target_os = "linux"))]
        let ray_tracing_properties: Option<RayTracingPipelineProperties> = None;

        // The host-pointer import alignment is a physical-device property;
        // well-defined to chain once the extension is advertised.
        #[cfg(any(target_os = "linux", target_os = "macos", target_os = "ios"))]
        let min_imported_host_pointer_alignment: u64 = if has_external_memory_host {
            let mut host_props =
                vk::PhysicalDeviceExternalMemoryHostPropertiesEXT::builder().build();
            let mut props2 = vk::PhysicalDeviceProperties2::builder()
                .push_next(&mut host_props)
                .build();
            unsafe { instance.get_physical_device_properties2(physical_device, &mut props2) };
            tracing::info!(
                min_imported_host_pointer_alignment =
                    host_props.min_imported_host_pointer_alignment,
                "VK_EXT_external_memory_host: host-pointer import alignment"
            );
            host_props.min_imported_host_pointer_alignment
        } else {
            0
        };

        // Dynamic rendering, timeline semaphores and synchronization2 are the
        // promoted-1.3 core the engine is written against; requesting them is
        // not optional on any platform. A device that refuses one
        // fails creation with FEATURE_NOT_PRESENT rather than being quietly
        // served a device the engine's call sites cannot use.
        let mut dynamic_rendering_features = vk::PhysicalDeviceDynamicRenderingFeatures::builder()
            .dynamic_rendering(true)
            .build();

        let mut timeline_semaphore_features =
            vk::PhysicalDeviceTimelineSemaphoreFeatures::builder()
                .timeline_semaphore(true)
                .build();

        let mut synchronization2_features = vk::PhysicalDeviceSynchronization2Features::builder()
            .synchronization2(true)
            .build();

        // samplerYcbcrConversion is required to create NV12 image views / samplers
        // with VK_KHR_sampler_ycbcr_conversion (core in 1.1) on the codec layer's path.
        // Without it, VUID-vkCreateSamplerYcbcrConversion-None-01648 fires every frame.
        let mut vulkan_1_1_features = vk::PhysicalDeviceVulkan11Features::builder()
            .sampler_ycbcr_conversion(true)
            .build();

        // Ray-tracing requires `bufferDeviceAddress` (core 1.2). Use the
        // standalone `VkPhysicalDeviceBufferDeviceAddressFeatures`
        // extension struct rather than `VkPhysicalDeviceVulkan12Features`
        // — Vulkan spec rejects mixing the 1.2 aggregate with the
        // standalone `*Features` structs it subsumes (timeline-semaphore
        // / synchronization2 are already pushed via their standalone
        // structs above; using the 1.2 aggregate here would trip
        // VUID-VkDeviceCreateInfo-pNext-02830).
        #[cfg(target_os = "linux")]
        let mut buffer_device_address_features =
            vk::PhysicalDeviceBufferDeviceAddressFeatures::builder()
                .buffer_device_address(true)
                .build();

        #[cfg(target_os = "linux")]
        let mut acceleration_structure_features =
            vk::PhysicalDeviceAccelerationStructureFeaturesKHR::builder()
                .acceleration_structure(true)
                .build();

        #[cfg(target_os = "linux")]
        let mut ray_tracing_pipeline_features =
            vk::PhysicalDeviceRayTracingPipelineFeaturesKHR::builder()
                .ray_tracing_pipeline(true)
                .build();

        // shaderStorageImage{Read,Write}WithoutFormat (core 1.0) — the
        // tone-curve kernel's storage images are declared with SPIR-V
        // format `Unknown` so one pipeline serves BGRA8 / RGBA8 /
        // RGBA16F views without tripping the storage-image
        // format-match rule. Requested unconditionally: universal on
        // the platform floor (NVIDIA, radv, anv, llvmpipe all
        // advertise both), and a driver without them fails device
        // creation with FEATURE_NOT_PRESENT rather than reading
        // undefined values per dispatch. The shader-side capability
        // pairing is locked by `vulkan_tone_mapper::tests::`
        // `tone_curve_spirv_declares_the_without_format_capabilities`.
        let enabled_device_features = vk::PhysicalDeviceFeatures::builder()
            .shader_storage_image_read_without_format(true)
            .shader_storage_image_write_without_format(true)
            .build();

        let device_create_info = {
            #[cfg_attr(
                not(target_os = "linux"),
                allow(unused_mut, reason = "only the Linux build mutates it")
            )]
            let mut builder = vk::DeviceCreateInfo::builder()
                .queue_create_infos(&queue_create_infos)
                .enabled_extension_names(&device_extensions)
                .enabled_features(&enabled_device_features)
                .push_next(&mut dynamic_rendering_features)
                .push_next(&mut timeline_semaphore_features)
                .push_next(&mut synchronization2_features)
                .push_next(&mut vulkan_1_1_features);
            #[cfg(target_os = "linux")]
            if has_ray_tracing_pipeline {
                builder = builder
                    .push_next(&mut buffer_device_address_features)
                    .push_next(&mut acceleration_structure_features)
                    .push_next(&mut ray_tracing_pipeline_features);
            }
            builder.build()
        };

        let logical_device_destroyed_unless_taken = VulkanLogicalDeviceDestroyedUnlessTaken {
            device: unsafe { instance.create_device(physical_device, &device_create_info, None) }
                .map_err(|vulkan_error| {
                VulkanDevicePassOverReason::LogicalDeviceCreationRefused { vulkan_error }
            })?,
        };
        let device = logical_device_destroyed_unless_taken.device();

        #[cfg(target_os = "macos")]
        let metal_working_set_of_the_device = {
            let metal_device =
                match MetalDeviceWhoseWorkingSetBoundsFrameAllocations::exported_from_moltenvk(
                    device,
                ) {
                    Some(metal_device) => Some(metal_device),
                    None => {
                        tracing::warn!(
                            "MoltenVK exported no MTLDevice (VK_EXT_metal_objects absent or \
                             declined); frame allocations are bounded by the working set of \
                             the system's default Metal device instead"
                        );
                        MetalDeviceWhoseWorkingSetBoundsFrameAllocations::the_systems_default()
                    }
                }
                .ok_or_else(|| {
                    Error::GpuError(
                        "no Metal device could be read for the working set that bounds frame \
                         allocations: MoltenVK exported none and the system has no default"
                            .into(),
                    )
                })?;
            MetalWorkingSetOfTheDevice::new(metal_device)
        };

        // 8. Get the graphics queue
        let queue = unsafe { device.get_device_queue(queue_family_index, 0) };

        // 8a2. Get the transfer queue (may be same as graphics if no dedicated transfer family)
        let transfer_queue = unsafe { device.get_device_queue(transfer_queue_family_index, 0) };

        // 8d. Get the dedicated compute queue (if available)
        let compute_queue = compute_queue_family_index
            .map(|cq_family| unsafe { device.get_device_queue(cq_family, 0) });

        // 8e. One completion timeline per queue
        let queue_submission_completion_timelines =
            HostVulkanQueueSubmissionCompletionTimelines::new(device)?;

        // 9. Query memory properties (raw-allocation type choice, heap of a type)
        let memory_properties =
            unsafe { instance.get_physical_device_memory_properties(physical_device) };

        // 10. Create VMA allocator for all GPU memory management
        //
        //     DMA-BUF export is NOT configured globally on VMA. Exportable allocations
        //     (pixel buffers, textures for IPC) go through dedicated VMA POOLS that
        //     carry VkExportMemoryAllocateInfo via pMemoryAllocateNext. This isolates
        //     export from the default pool — internal allocations (display textures,
        //     compute images) use the default pool which has NO export flags. NVIDIA
        //     rejects new DMA-BUF exportable DEVICE_LOCAL block allocations after
        //     swapchain creation, so keeping internal allocations export-free is the
        //     correct fix.
        let mut alloc_options = vma::AllocatorOptions::new(instance, device, physical_device);
        alloc_options.version = vulkanalia::Version::new(1, 4, 0);
        // BUFFER_DEVICE_ADDRESS — required so buffers created with
        // `VK_BUFFER_USAGE_SHADER_DEVICE_ADDRESS_BIT` (acceleration-
        // structure storage / scratch / vertex / index, the SBT) get
        // `VK_MEMORY_ALLOCATE_DEVICE_ADDRESS_BIT` on their device-memory
        // allocations. Only enabled when the RT extension chain is on,
        // since the underlying `bufferDeviceAddress` feature is gated
        // there. Non-RT devices keep the default flags so the
        // allocator's behavior is unchanged on hardware without RT.
        #[cfg(target_os = "linux")]
        if has_ray_tracing_pipeline {
            alloc_options.flags |= vma::AllocatorCreateFlags::BUFFER_DEVICE_ADDRESS;
        }
        if has_memory_budget {
            alloc_options.flags |= vma::AllocatorCreateFlags::EXT_MEMORY_BUDGET;
        }

        let allocator = Arc::new(
            unsafe { vma::Allocator::new(&alloc_options) }
                .map_err(|e| Error::GpuError(format!("Failed to create VMA allocator: {e}")))?,
        );

        // Run the EGL probe BEFORE creating pools — the tiled pool needs the
        // modifier list. Probe failure (no libEGL, no display, extension
        // missing) is non-fatal: the table stays empty and the tiled pool
        // is skipped, which makes render-target DMA-BUF allocations refuse
        // with an error rather than silently picking a sampler-only modifier.
        // NVIDIA EGL samples linear DMA-BUFs only. The table
        // is the EGL device's on this device's render node, and a device that
        // exports no DMA-BUF takes none.
        #[cfg(target_os = "linux")]
        let drm_modifier_table = if supports_dma_buf_export {
            let render_node = drm_render_node_number_of_the_physical_device(
                instance,
                physical_device,
                available_device_ext_names.contains(&c"VK_EXT_physical_device_drm"),
            );
            match drm_modifier_probe::drm_modifier_table_of_the_render_node_probed_once_per_process(
                render_node,
            ) {
                Ok(t) => t,
                Err(e) => {
                    tracing::warn!(
                        "EGL DRM modifier probe failed: {e} — render-target DMA-BUF \
                         pool will be unavailable; only sampler-only / linear DMA-BUF \
                         allocations will succeed"
                    );
                    Arc::new(DrmModifierTable::empty())
                }
            }
        } else {
            Arc::new(DrmModifierTable::empty())
        };

        // Build the OPAQUE_FD buffer pool — used by CUDA / OpenCL interop
        // (host allocates, exports OPAQUE_FD, CUDA `cudaImportExternalMemory`
        // imports it). Independent of the DMA-BUF pools below; needs its
        // own pool because VMA carries one chained `VkExportMemoryAllocateInfo`
        // per pool. Probe failure is non-fatal — callers refuse the
        // allocation if the pool isn't available.
        #[cfg(target_os = "linux")]
        let (opaque_fd_buffer_pool, opaque_fd_buffer_export_info) = if supports_external_memory {
            match Self::create_opaque_fd_buffer_pool(&allocator) {
                Ok((pool, export_info)) => (Some(pool), Some(export_info)),
                Err(e) => {
                    tracing::warn!(
                        "OPAQUE_FD buffer pool creation failed — CUDA/OpenCL interop will be unavailable: {e}"
                    );
                    (None, None)
                }
            }
        } else {
            (None, None)
        };

        // Host-cached OPAQUE_FD buffer pool — the CPU-read sibling of the
        // pool above, used by the surface-export staging's HostVisible
        // residency. Absence is a real answer, not a failure: allocation
        // degrades to the write-combined pool, which the plan requires be
        // slower but never refused.
        #[cfg(target_os = "linux")]
        let (opaque_fd_buffer_pool_host_cached, opaque_fd_buffer_export_info_host_cached) =
            if supports_external_memory {
                match Self::create_opaque_fd_buffer_pool_host_cached(&allocator) {
                    Ok(Some((pool, export_info))) => (Some(pool), Some(export_info)),
                    Ok(None) => {
                        tracing::warn!(
                            "no cached exportable host-visible memory type on this device — \
                             CPU readback of a texture-backed surface keeps the \
                             write-combined staging and reads it far more slowly"
                        );
                        (None, None)
                    }
                    Err(e) => {
                        tracing::warn!(
                            "host-cached OPAQUE_FD buffer pool creation failed — CPU readback \
                             staging falls back to the write-combined pool: {e}"
                        );
                        (None, None)
                    }
                }
            } else {
                (None, None)
            };

        // DEVICE_LOCAL OPAQUE_FD pool — the GPU-resident sibling. Pool
        // construction failure is non-fatal in the same way as the
        // HOST_VISIBLE pool: callers refuse the allocation when the pool
        // isn't available. Older drivers without external-memory support
        // skip this entirely.
        #[cfg(target_os = "linux")]
        let (opaque_fd_buffer_pool_device_local, opaque_fd_buffer_export_info_device_local) =
            if supports_external_memory {
                match Self::create_opaque_fd_buffer_pool_device_local(&allocator) {
                    Ok((pool, export_info)) => (Some(pool), Some(export_info)),
                    Err(e) => {
                        tracing::warn!(
                            "DEVICE_LOCAL OPAQUE_FD buffer pool creation failed — \
                             GPU-resident CUDA interop will be unavailable: {e}"
                        );
                        (None, None)
                    }
                }
            } else {
                (None, None)
            };

        // OPAQUE_FD image pool — DEVICE_LOCAL, OPTIMAL tiling.
        // Image-flavored sibling of the buffer pools above;
        // unblocks `cudaExternalMemoryGetMappedMipmappedArray` for the
        // tiled-image zero-copy Vulkan → CUDA path. Pool construction
        // failure is non-fatal: callers (`HostVulkanTexture::new_opaque_fd_export`)
        // refuse the allocation when the pool isn't available. Older
        // drivers without external-memory support skip this entirely.
        #[cfg(target_os = "linux")]
        let (opaque_fd_image_pool, opaque_fd_image_export_info) = if supports_external_memory {
            match Self::create_opaque_fd_image_pool(&allocator) {
                Ok((pool, export_info)) => (Some(pool), Some(export_info)),
                Err(e) => {
                    tracing::warn!(
                        "OPAQUE_FD image pool creation failed — CUDA `VkImage` interop \
                         (mipmapped-array import) will be unavailable: {e}"
                    );
                    (None, None)
                }
            }
        } else {
            (None, None)
        };

        // Build DMA-BUF export pools only on a device that exports DMA-BUF.
        // The tiled pool is built only when the EGL probe found at least one
        // RT-capable modifier for BGRA — that's the probe shape.
        #[cfg(target_os = "linux")]
        let (
            dma_buf_buffer_pool,
            dma_buf_image_pool,
            dma_buf_image_pool_tiled,
            dma_buf_buffer_export_info,
            dma_buf_image_export_info,
            dma_buf_image_tiled_export_info,
        ) = if supports_dma_buf_export {
            // BGRA8_UNORM in Vulkan = ARGB8888 in DRM.
            let bgra_modifiers =
                drm_modifier_table.rt_modifiers(drm_modifier_probe::fourcc::DRM_FORMAT_ARGB8888);
            match Self::create_dma_buf_pools(&allocator, device, &memory_properties, bgra_modifiers)
            {
                Ok((bp, ip, tp, bi, ii, ti)) => (Some(bp), Some(ip), tp, Some(bi), Some(ii), ti),
                Err(e) => {
                    tracing::warn!(
                        "DMA-BUF export pools could not be created — falling back to \
                         default pool for exportable allocations (may fail on NVIDIA \
                         after swapchain creation): {e}"
                    );
                    (None, None, None, None, None, None)
                }
            }
        } else {
            (None, None, None, None, None, None)
        };

        tracing::info!(
            "Vulkan device initialized: {} (queue family {}, {} memory types, external_memory={}, cross_device_dma_buf_probe={}, acquire_unmodified={}, ray_tracing={}, vma=enabled, dma_buf_pools={})",
            device_name,
            queue_family_index,
            memory_properties.memory_type_count,
            supports_external_memory,
            supports_cross_device_dma_buf_probe,
            has_acquire_unmodified,
            has_ray_tracing_pipeline,
            {
                #[cfg(target_os = "linux")]
                {
                    dma_buf_buffer_pool.is_some()
                }
                #[cfg(not(target_os = "linux"))]
                {
                    false
                }
            }
        );

        let frame_allocation_device_memory_budget_gate =
            FrameAllocationDeviceMemoryBudgetGate::new();
        let device = Self {
            vulkan_instance: Arc::clone(vulkan_instance),
            physical_device,
            memory_properties,
            max_memory_allocation_count,
            #[cfg(target_os = "linux")]
            has_memory_budget,
            frame_allocation_device_memory_budget_gate,
            #[cfg(target_os = "linux")]
            device_memory_budget_fresh_fetch_frame_index: std::sync::atomic::AtomicU32::new(0),
            #[cfg(target_os = "macos")]
            metal_working_set_of_the_device,
            device: logical_device_destroyed_unless_taken.take_the_logical_device(),
            queue,
            queue_family_index,
            transfer_queue_family_index,
            transfer_queue,
            device_name,
            supports_external_memory,
            device_is_software,
            supports_dma_buf_export,
            supports_semaphore_export,
            supports_cross_device_dma_buf_probe,
            has_acquire_unmodified,
            has_external_memory_host,
            min_imported_host_pointer_alignment,
            has_ray_tracing_pipeline,
            ray_tracing_properties,
            subgroup_operation_support,
            compute_queue_family_index,
            compute_queue,
            allocator: Some(allocator),
            #[cfg(target_os = "linux")]
            dma_buf_buffer_pool,
            #[cfg(target_os = "linux")]
            dma_buf_image_pool,
            #[cfg(target_os = "linux")]
            dma_buf_image_pool_tiled,
            #[cfg(target_os = "linux")]
            _dma_buf_buffer_export_info: dma_buf_buffer_export_info,
            #[cfg(target_os = "linux")]
            _dma_buf_image_export_info: dma_buf_image_export_info,
            #[cfg(target_os = "linux")]
            _dma_buf_image_tiled_export_info: dma_buf_image_tiled_export_info,
            #[cfg(target_os = "linux")]
            opaque_fd_buffer_pool,
            #[cfg(target_os = "linux")]
            opaque_fd_buffer_pool_host_cached,
            #[cfg(target_os = "linux")]
            opaque_fd_buffer_pool_device_local,
            #[cfg(target_os = "linux")]
            opaque_fd_image_pool,
            #[cfg(target_os = "linux")]
            _opaque_fd_buffer_export_info: opaque_fd_buffer_export_info,
            #[cfg(target_os = "linux")]
            _opaque_fd_buffer_export_info_host_cached: opaque_fd_buffer_export_info_host_cached,
            #[cfg(target_os = "linux")]
            _opaque_fd_buffer_export_info_device_local: opaque_fd_buffer_export_info_device_local,
            #[cfg(target_os = "linux")]
            _opaque_fd_image_export_info: opaque_fd_image_export_info,
            physical_device_uuid,
            min_storage_buffer_offset_alignment,
            #[cfg(target_os = "linux")]
            drm_modifier_table,
            live_allocation_count: AtomicUsize::new(0),
            queue_submission_count: AtomicUsize::new(0),
            counted_host_waits_on_gpu_work_count: AtomicUsize::new(0),
            queue_submission_completion_timelines: Some(queue_submission_completion_timelines),
            graphics_queue_mutex: Mutex::new(()),
            transfer_queue_mutex: Mutex::new(()),
            compute_queue_mutex: Mutex::new(()),
            #[cfg(target_os = "linux")]
            export_pool_sentinels: Vec::new(),
        };

        // Wrapped in `Arc` before the pipeline-compiler pre-warm, which
        // builds its probe kernels through constructors taking
        // `&Arc<HostVulkanDevice>`.
        #[cfg(not(target_os = "linux"))]
        let device = Arc::new(device);

        #[cfg(target_os = "linux")]
        let device = {
            let mut device = Arc::new(device);
            let sentinels =
                Self::allocate_one_sentinel_per_export_pool(&device).map_err(|refusal| {
                    VulkanDevicePassOverReason::ExportPoolSentinelAllocationFailed {
                        refusal: refusal.to_string(),
                    }
                })?;
            // Sentinels hold the allocator, not `Arc<HostVulkanDevice>`
            // clones, so the Arc strong count is still 1 here and
            // `get_mut` succeeds.
            Arc::get_mut(&mut device)
                .expect(
                    "HostVulkanDevice has unique Arc ownership during construction; \
                     export sentinels must not hold Arc<HostVulkanDevice> clones",
                )
                .export_pool_sentinels = sentinels;
            Self::prewarm_pipeline_compiler(&device);
            Self::prewarm_graphics_pipeline(&device);
            device
        };

        Ok(device)
    }
}
