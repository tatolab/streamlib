// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::sync::atomic::Ordering;

use crate::core::{Error, Result};
use crate::vulkan::rhi::memory_and_buffers::vulkan_frame_allocation_device_memory_budget::{
    DeviceMemoryAllocationCountReading, FrameAllocationBudgetReading,
    FrameAllocationDeviceMemoryReading, FrameDeviceMemoryRequest,
    device_memory_allocations_this_process_holds,
};
#[cfg(test)]
use crate::vulkan::rhi::memory_and_buffers::vulkan_metal_working_set_budget::MetalWorkingSetOfTheDevice;

use super::HostVulkanDevice;

impl HostVulkanDevice {
    /// The device memory one frame allocation is decided on, read for it now:
    /// Metal's working set read live, whatever the frame's placement.
    pub(super) fn read_the_frame_allocation_device_memory_fetched_now(
        &self,
        _request: &FrameDeviceMemoryRequest,
    ) -> Result<FrameAllocationDeviceMemoryReading> {
        let heap_budgets = self
            .allocator()
            .get_heap_budgets()
            .map_err(|e| Error::GpuError(format!("vmaGetHeapBudgets failed: {e}")))?;
        Ok(FrameAllocationDeviceMemoryReading {
            budget: FrameAllocationBudgetReading::MetalWorkingSet(
                self.metal_working_set_of_the_device.reading(),
            ),
            allocation_count: DeviceMemoryAllocationCountReading {
                live_device_memory_allocation_count: device_memory_allocations_this_process_holds(
                    heap_budgets
                        .iter()
                        .map(|heap_budget| heap_budget.statistics.blockCount),
                    self.live_allocation_count.load(Ordering::Relaxed),
                ),
                max_memory_allocation_count: self.max_memory_allocation_count,
            },
        })
    }

    /// The Metal working set frame allocations are weighed against.
    #[cfg(test)]
    pub(crate) fn metal_working_set_of_the_device(&self) -> &MetalWorkingSetOfTheDevice {
        &self.metal_working_set_of_the_device
    }
}
