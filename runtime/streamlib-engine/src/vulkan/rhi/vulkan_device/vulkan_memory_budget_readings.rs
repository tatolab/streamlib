// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::sync::atomic::Ordering;

use vulkanalia::vk;

use crate::core::{Error, Result};
use crate::vulkan::rhi::vulkan_frame_allocation_device_memory_budget::{
    DeviceMemoryAllocationCountReading, DeviceMemoryHeapBudgetReading,
    FrameAllocationBudgetReading, FrameAllocationDeviceMemoryReading, FrameDeviceMemoryPlacement,
    FrameDeviceMemoryRequest, device_memory_allocations_this_process_holds,
    heap_budget_without_the_memory_budget_extension,
};

use super::HostVulkanDevice;

impl HostVulkanDevice {
    /// The heap a memory type allocates from; refused by name past the
    /// device's memory type count.
    pub(crate) fn device_memory_heap_of_memory_type(&self, memory_type_index: u32) -> Result<u32> {
        if memory_type_index >= self.memory_properties.memory_type_count {
            return Err(Error::GpuError(format!(
                "memory type {memory_type_index} is past this device's {} memory types",
                self.memory_properties.memory_type_count
            )));
        }
        Ok(self.memory_properties.memory_types[memory_type_index as usize].heap_index)
    }

    /// The property flags of the memory type at `memory_type_index`, or
    /// `None` past the device's memory type count.
    pub(crate) fn property_flags_of_memory_type_index(
        &self,
        memory_type_index: u32,
    ) -> Option<vk::MemoryPropertyFlags> {
        (memory_type_index < self.memory_properties.memory_type_count)
            .then(|| self.memory_properties.memory_types[memory_type_index as usize].property_flags)
    }

    /// Every heap's budget, usage and allocator holdings, with the budget
    /// fetched from the driver for this call: VMA otherwise serves a budget
    /// it refreshes only every 30 of its own operations.
    pub(crate) fn read_every_heaps_device_memory_budget_fetched_from_the_driver_now(
        &self,
    ) -> Result<Vec<DeviceMemoryHeapBudgetReading>> {
        use crate::core::error::{DeviceMemoryHeapBudgetSource, DeviceMemoryHeapLocality};

        let allocator = self.allocator();
        let fresh_fetch_frame_index = self
            .device_memory_budget_fresh_fetch_frame_index
            .fetch_add(1, Ordering::Relaxed)
            .wrapping_add(1);
        // SAFETY: the frame index is opaque to VMA; with EXT_MEMORY_BUDGET
        // enabled, setting it fetches every heap's budget from the driver.
        unsafe { allocator.set_current_frame_index(fresh_fetch_frame_index) };
        let heap_budgets = allocator
            .get_heap_budgets()
            .map_err(|e| Error::GpuError(format!("vmaGetHeapBudgets failed: {e}")))?;
        let heap_locality_of = |heap_index: usize| {
            if self.memory_properties.memory_heaps[heap_index]
                .flags
                .contains(vk::MemoryHeapFlags::DEVICE_LOCAL)
            {
                DeviceMemoryHeapLocality::DeviceLocal
            } else {
                DeviceMemoryHeapLocality::NotDeviceLocal
            }
        };
        let system_memory_available_bytes = (!self.has_memory_budget
            && (0..heap_budgets.len())
                .any(|heap_index| heap_locality_of(heap_index) == DeviceMemoryHeapLocality::NotDeviceLocal))
        .then(|| {
            crate::linux::system_memory_available_from_proc_meminfo::read_system_memory_available_bytes_from_proc_meminfo()
                .inspect_err(|e| {
                    tracing::debug!(
                        error = %e,
                        "the system's available memory is unreadable; a host heap's budget stays \
                         the allocator's estimate"
                    );
                })
                .ok()
        })
        .flatten();
        Ok(heap_budgets
            .iter()
            .enumerate()
            .map(|(heap_index, heap_budget)| {
                let heap_locality = heap_locality_of(heap_index);
                let (heap_budget_bytes, heap_budget_source) = if self.has_memory_budget {
                    (
                        heap_budget.budget,
                        DeviceMemoryHeapBudgetSource::ReportedByTheDriver,
                    )
                } else {
                    heap_budget_without_the_memory_budget_extension(
                        heap_budget.budget,
                        heap_budget.usage,
                        heap_locality,
                        system_memory_available_bytes,
                    )
                };
                DeviceMemoryHeapBudgetReading {
                    heap_index: heap_index as u32,
                    heap_locality,
                    heap_budget_bytes,
                    heap_usage_bytes: heap_budget.usage,
                    bytes_this_runtimes_allocator_pools_hold_on_the_heap: heap_budget
                        .statistics
                        .blockBytes,
                    device_memory_blocks_this_runtimes_allocator_holds_on_the_heap: heap_budget
                        .statistics
                        .blockCount,
                    heap_budget_source,
                }
            })
            .collect())
    }

    /// The device memory one frame allocation is decided on, read for it now.
    pub(super) fn read_the_frame_allocation_device_memory_fetched_now(
        &self,
        request: &FrameDeviceMemoryRequest,
    ) -> Result<FrameAllocationDeviceMemoryReading> {
        let FrameDeviceMemoryPlacement::VulkanMemoryType { memory_type_index } = request.placement;
        let heap_index = self.device_memory_heap_of_memory_type(memory_type_index)?;
        let every_heaps_reading =
            self.read_every_heaps_device_memory_budget_fetched_from_the_driver_now()?;
        let allocation_count = DeviceMemoryAllocationCountReading {
            live_device_memory_allocation_count: device_memory_allocations_this_process_holds(
                every_heaps_reading.iter().map(|heap| {
                    heap.device_memory_blocks_this_runtimes_allocator_holds_on_the_heap
                }),
                self.live_allocation_count.load(Ordering::Relaxed),
            ),
            max_memory_allocation_count: self.max_memory_allocation_count,
        };
        let heap_reading = every_heaps_reading
            .into_iter()
            .nth(heap_index as usize)
            .ok_or_else(|| {
                Error::GpuError(format!(
                    "the allocator reported no budget for heap {heap_index} of memory type \
                     {memory_type_index}"
                ))
            })?;
        Ok(FrameAllocationDeviceMemoryReading {
            budget: FrameAllocationBudgetReading::VulkanMemoryHeap(heap_reading),
            allocation_count,
        })
    }
}
