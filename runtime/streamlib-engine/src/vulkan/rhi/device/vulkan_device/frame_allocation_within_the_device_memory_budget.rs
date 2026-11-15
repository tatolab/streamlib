// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use vma::Alloc as _;
use vulkanalia::prelude::v1_4::*;
use vulkanalia::vk;
use vulkanalia_vma as vma;

use crate::core::{Error, Result};
use crate::vulkan::rhi::memory_and_buffers::vulkan_frame_allocation_device_memory_budget::{
    DeviceMemoryBudgetAdmission, FrameDeviceMemoryRequest,
};

#[cfg(target_os = "linux")]
use super::export_memory_pools::refuse_an_image_the_export_pools_memory_type_cannot_bind;
use super::{
    HostVulkanDevice, VmaFrameAllocationSource, frame_device_memory_request_on_a_vulkan_memory_type,
};

impl HostVulkanDevice {
    /// Allocate as `admission` says: a frame through the device-memory budget
    /// gate, refused by name past its budget or near the allocation-count
    /// limit; an exempt engine allocation straight through, never weighed.
    pub(crate) fn allocate_as_the_device_memory_budget_admits<Allocated>(
        &self,
        admission: DeviceMemoryBudgetAdmission,
        frame_device_memory_request: impl FnOnce() -> Result<FrameDeviceMemoryRequest>,
        allocate: impl FnOnce() -> Result<Allocated>,
    ) -> Result<Allocated> {
        match admission {
            DeviceMemoryBudgetAdmission::EngineAllocationTheDeviceMemoryBudgetNeverRefuses => {
                allocate()
            }
            DeviceMemoryBudgetAdmission::FrameRefusedPastTheDeviceMemoryBudget => {
                let request = frame_device_memory_request()?;
                self.frame_allocation_device_memory_budget_gate
                    .allocate_a_frame_within_the_device_memory_budget(
                        &request,
                        || self.read_the_frame_allocation_device_memory_fetched_now(&request),
                        allocate,
                    )
            }
        }
    }

    /// Create a frame image from `allocation_source`, weighed as `admission`
    /// says against the heap that same source allocates it from.
    pub(crate) fn create_frame_image_within_the_device_memory_budget(
        &self,
        admission: DeviceMemoryBudgetAdmission,
        allocation_source: VmaFrameAllocationSource,
        image_create_info: &vk::ImageCreateInfo,
        allocation_options: &vma::AllocationOptions,
        frame_allocation_site: &'static str,
        describe_the_allocators_refusal: impl FnOnce(vk::ErrorCode) -> Error,
    ) -> Result<(vk::Image, vma::Allocation)> {
        // SAFETY (every unsafe block below): the memory-type queries only read
        // the allocator's types, and the create info and options are the
        // caller's, valid on this device.
        match allocation_source {
            #[cfg(target_os = "linux")]
            VmaFrameAllocationSource::ExportPool(export_pool) => {
                let memory_requirements =
                    self.memory_requirements_of_an_image_create_info(image_create_info)?;
                refuse_an_image_the_export_pools_memory_type_cannot_bind(
                    memory_requirements.memory_type_bits,
                    export_pool.memory_type_index,
                    image_create_info,
                    frame_allocation_site,
                )?;
                self.allocate_as_the_device_memory_budget_admits(
                    admission,
                    || {
                        frame_device_memory_request_on_a_vulkan_memory_type(
                            memory_requirements,
                            Ok(export_pool.memory_type_index),
                            frame_allocation_site,
                        )
                    },
                    || {
                        unsafe {
                            export_pool
                                .pool
                                .create_image(*image_create_info, allocation_options)
                        }
                        .map_err(describe_the_allocators_refusal)
                    },
                )
            }
            VmaFrameAllocationSource::DefaultAllocator => self
                .allocate_as_the_device_memory_budget_admits(
                    admission,
                    || {
                        let memory_requirements =
                            self.memory_requirements_of_an_image_create_info(image_create_info)?;
                        // VMA cannot infer a DRM-modifier image's usage for an
                        // `Auto` query, so the type is chosen on the options'
                        // flags alone, which every frame constructor states.
                        let memory_type_index_or_refusal = if image_create_info.tiling
                            == vk::ImageTiling::DRM_FORMAT_MODIFIER_EXT
                        {
                            unsafe {
                                self.allocator().find_memory_type_index(
                                    memory_requirements.memory_type_bits,
                                    &vma::AllocationOptions {
                                        usage: vma::MemoryUsage::Unknown,
                                        ..*allocation_options
                                    },
                                )
                            }
                        } else {
                            unsafe {
                                self.allocator().find_memory_type_index_for_image_info(
                                    *image_create_info,
                                    allocation_options,
                                )
                            }
                        };
                        frame_device_memory_request_on_a_vulkan_memory_type(
                            memory_requirements,
                            memory_type_index_or_refusal,
                            frame_allocation_site,
                        )
                    },
                    || {
                        unsafe {
                            self.allocator()
                                .create_image(*image_create_info, allocation_options)
                        }
                        .map_err(describe_the_allocators_refusal)
                    },
                ),
        }
    }

    /// Create a frame buffer from `allocation_source`, weighed as `admission`
    /// says against the heap that same source allocates it from.
    pub(crate) fn create_frame_buffer_within_the_device_memory_budget(
        &self,
        admission: DeviceMemoryBudgetAdmission,
        allocation_source: VmaFrameAllocationSource,
        buffer_create_info: &vk::BufferCreateInfo,
        allocation_options: &vma::AllocationOptions,
        frame_allocation_site: &'static str,
        describe_the_allocators_refusal: impl FnOnce(vk::ErrorCode) -> Error,
    ) -> Result<(vk::Buffer, vma::Allocation)> {
        // SAFETY (every unsafe block below): the memory-type query only reads
        // the allocator's types, and the create info and options are the
        // caller's, valid on this device.
        match allocation_source {
            #[cfg(target_os = "linux")]
            VmaFrameAllocationSource::ExportPool(export_pool) => self
                .allocate_as_the_device_memory_budget_admits(
                    admission,
                    || {
                        frame_device_memory_request_on_a_vulkan_memory_type(
                            self.memory_requirements_of_a_buffer_create_info(buffer_create_info),
                            Ok(export_pool.memory_type_index),
                            frame_allocation_site,
                        )
                    },
                    || {
                        unsafe {
                            export_pool
                                .pool
                                .create_buffer(*buffer_create_info, allocation_options)
                        }
                        .map_err(describe_the_allocators_refusal)
                    },
                ),
            VmaFrameAllocationSource::DefaultAllocator => self
                .allocate_as_the_device_memory_budget_admits(
                    admission,
                    || {
                        frame_device_memory_request_on_a_vulkan_memory_type(
                            self.memory_requirements_of_a_buffer_create_info(buffer_create_info),
                            unsafe {
                                self.allocator().find_memory_type_index_for_buffer_info(
                                    *buffer_create_info,
                                    allocation_options,
                                )
                            },
                            frame_allocation_site,
                        )
                    },
                    || {
                        unsafe {
                            self.allocator()
                                .create_buffer(*buffer_create_info, allocation_options)
                        }
                        .map_err(describe_the_allocators_refusal)
                    },
                ),
        }
    }

    /// Memory requirements of an image not yet created. A DRM-modifier image
    /// takes a probe image: `vkGetDeviceImageMemoryRequirements` needs a
    /// `planeAspect` naming a memory plane of the modifier the driver picks
    /// only at creation (VUID-VkDeviceImageMemoryRequirements-pCreateInfo-06420).
    fn memory_requirements_of_an_image_create_info(
        &self,
        image_create_info: &vk::ImageCreateInfo,
    ) -> Result<vk::MemoryRequirements> {
        if image_create_info.tiling == vk::ImageTiling::DRM_FORMAT_MODIFIER_EXT {
            // SAFETY: the create info is the one the caller is about to
            // allocate with; the probe image is destroyed before returning.
            let probe_image = unsafe { self.device.create_image(image_create_info, None) }
                .map_err(|e| {
                    Error::GpuError(format!(
                        "vkCreateImage for the memory-requirements probe of a DRM-modifier \
                         image failed: {e}"
                    ))
                })?;
            let memory_requirements =
                unsafe { self.device.get_image_memory_requirements(probe_image) };
            unsafe { self.device.destroy_image(probe_image, None) };
            return Ok(memory_requirements);
        }
        let image_memory_requirements_info = vk::DeviceImageMemoryRequirements::builder()
            .create_info(image_create_info)
            .build();
        let mut memory_requirements = vk::MemoryRequirements2::default();
        // SAFETY: a query on a valid create info; nothing is created.
        unsafe {
            self.device.get_device_image_memory_requirements(
                &image_memory_requirements_info,
                &mut memory_requirements,
            )
        };
        Ok(memory_requirements.memory_requirements)
    }

    /// Memory requirements of a buffer not yet created.
    fn memory_requirements_of_a_buffer_create_info(
        &self,
        buffer_create_info: &vk::BufferCreateInfo,
    ) -> vk::MemoryRequirements {
        let buffer_memory_requirements_info = vk::DeviceBufferMemoryRequirements::builder()
            .create_info(buffer_create_info)
            .build();
        let mut memory_requirements = vk::MemoryRequirements2::default();
        // SAFETY: a query on a valid create info; nothing is created.
        unsafe {
            self.device.get_device_buffer_memory_requirements(
                &buffer_memory_requirements_info,
                &mut memory_requirements,
            )
        };
        memory_requirements.memory_requirements
    }

    /// Until the returned guard drops, weigh every frame allocation on this
    /// device against a budget lowered to its usage, so each one is refused.
    #[cfg(test)]
    pub(crate) fn leave_no_room_in_any_frame_allocation_budget_for_this_test(
        &self,
    ) -> crate::vulkan::rhi::memory_and_buffers::vulkan_frame_allocation_device_memory_budget::FrameAllocationBudgetLeftWithNoRoomUntilDropped<'_>
{
        self.frame_allocation_device_memory_budget_gate
            .leave_no_room_in_any_budget_for_this_test()
    }
}
