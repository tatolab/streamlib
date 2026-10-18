// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The device-memory bound every frame allocation is weighed against before it
//! allocates: its heap's budget less a fixed headroom (on macOS, Metal's
//! recommended working set), and the device's `maxMemoryAllocationCount`.

use std::sync::{Mutex, PoisonError};

#[cfg(any(target_os = "linux", test))]
use crate::core::error::{DeviceMemoryHeapBudgetSource, DeviceMemoryHeapLocality};
use crate::core::{Error, Result};

/// The room a frame allocation's own request must leave under its heap's
/// budget, for codecs and other processes.
pub(crate) const HEADROOM_BYTES_A_FRAME_REQUEST_MUST_LEAVE_UNDER_THE_DEVICE_MEMORY_BUDGET: u64 =
    64 * 1024 * 1024;

/// Device memory allocations a frame allocation leaves in reserve under
/// `maxMemoryAllocationCount`.
pub(crate) const DEVICE_MEMORY_ALLOCATIONS_KEPT_IN_RESERVE_UNDER_THE_DEVICE_LIMIT: u32 = 128;

/// Whether the device-memory budget may refuse an allocation a constructor makes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DeviceMemoryBudgetAdmission {
    /// A frame: refused by name past the budget or near the allocation-count limit.
    FrameRefusedPastTheDeviceMemoryBudget,
    /// Engine memory the budget never weighs: an export-pool probe at device
    /// creation, or a uniform, vertex or index buffer.
    EngineAllocationTheDeviceMemoryBudgetNeverRefuses,
}

/// One frame allocation's claim on device memory, derived from the create-info
/// it allocates with.
#[derive(Clone, Debug)]
pub(crate) struct FrameDeviceMemoryRequest {
    pub(crate) requested_bytes: u64,
    pub(crate) placement: FrameDeviceMemoryPlacement,
    pub(crate) frame_allocation_site: &'static str,
}

/// Where a frame's memory comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FrameDeviceMemoryPlacement {
    /// A Vulkan memory type, and through it one heap.
    VulkanMemoryType { memory_type_index: u32 },
    /// A private IOSurface the runtime mints, which allocates when it is created.
    #[cfg(target_os = "macos")]
    PrivateIOSurface,
}

impl std::fmt::Display for FrameDeviceMemoryPlacement {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::VulkanMemoryType { memory_type_index } => {
                write!(f, "Vulkan memory type {memory_type_index}")
            }
            #[cfg(target_os = "macos")]
            Self::PrivateIOSurface => f.write_str("a private IOSurface"),
        }
    }
}

/// The budget one frame allocation is weighed against.
#[derive(Clone, Debug)]
pub(crate) enum FrameAllocationBudgetReading {
    #[cfg(any(target_os = "linux", test))]
    VulkanMemoryHeap(DeviceMemoryHeapBudgetReading),
    #[cfg(any(target_os = "macos", test))]
    MetalWorkingSet(MetalWorkingSetBudgetReading),
}

/// One device memory heap's budget, usage and the bytes this runtime's
/// allocator holds on it.
#[cfg(any(target_os = "linux", test))]
#[derive(Clone, Debug)]
pub(crate) struct DeviceMemoryHeapBudgetReading {
    pub(crate) heap_index: u32,
    pub(crate) heap_locality: DeviceMemoryHeapLocality,
    pub(crate) heap_budget_bytes: u64,
    pub(crate) heap_usage_bytes: u64,
    /// VMA's `blockBytes`: every `VkDeviceMemory` the default and export pools hold on the heap.
    pub(crate) bytes_this_runtimes_allocator_pools_hold_on_the_heap: u64,
    /// VMA's `blockCount`: the `VkDeviceMemory` objects the allocator holds on the heap.
    pub(crate) device_memory_blocks_this_runtimes_allocator_holds_on_the_heap: u32,
    pub(crate) heap_budget_source: DeviceMemoryHeapBudgetSource,
}

/// Metal's recommended working set and the bytes the Metal device has allocated of it.
#[cfg(any(target_os = "macos", test))]
#[derive(Clone, Debug)]
pub(crate) struct MetalWorkingSetBudgetReading {
    pub(crate) recommended_max_working_set_bytes: u64,
    /// `currentAllocatedSize`, the working set's usage. It already counts every
    /// private IOSurface the runtime mints: MoltenVK wraps each at once in an
    /// `MTLTexture` over the surface or a no-copy `MTLBuffer` over its pages.
    pub(crate) metal_current_allocated_bytes: u64,
}

/// The `VkDeviceMemory` objects this process holds against the device's limit.
#[derive(Clone, Copy, Debug)]
pub(crate) struct DeviceMemoryAllocationCountReading {
    pub(crate) live_device_memory_allocation_count: u64,
    pub(crate) max_memory_allocation_count: u32,
}

/// Everything one frame allocation is decided on, read for that allocation.
#[derive(Clone, Debug)]
pub(crate) struct FrameAllocationDeviceMemoryReading {
    pub(crate) budget: FrameAllocationBudgetReading,
    pub(crate) allocation_count: DeviceMemoryAllocationCountReading,
}

impl FrameAllocationDeviceMemoryReading {
    /// The same reading with every budget lowered to its usage.
    #[cfg(test)]
    fn with_no_room_left_in_its_budget(mut self) -> Self {
        match &mut self.budget {
            FrameAllocationBudgetReading::VulkanMemoryHeap(heap) => {
                heap.heap_budget_bytes = heap.heap_usage_bytes;
            }
            FrameAllocationBudgetReading::MetalWorkingSet(working_set) => {
                working_set.recommended_max_working_set_bytes =
                    working_set.metal_current_allocated_bytes;
            }
        }
        self
    }
}

/// Whether `requested_bytes` on top of `usage_bytes` would leave less than the
/// headroom under `budget_bytes`.
fn request_leaves_less_than_the_headroom(
    budget_bytes: u64,
    usage_bytes: u64,
    requested_bytes: u64,
) -> bool {
    usage_bytes.saturating_add(requested_bytes)
        > budget_bytes.saturating_sub(
            HEADROOM_BYTES_A_FRAME_REQUEST_MUST_LEAVE_UNDER_THE_DEVICE_MEMORY_BUDGET,
        )
}

/// The refusal a frame allocation meets against `reading`, or `None` when it
/// fits. The budget is weighed before the allocation count.
pub(crate) fn refusal_of_a_frame_allocation_past_the_device_memory_bound(
    request: &FrameDeviceMemoryRequest,
    reading: &FrameAllocationDeviceMemoryReading,
) -> Option<Error> {
    let budget_refusal = match &reading.budget {
        #[cfg(any(target_os = "linux", test))]
        FrameAllocationBudgetReading::VulkanMemoryHeap(heap) => {
            request_leaves_less_than_the_headroom(
                heap.heap_budget_bytes,
                heap.heap_usage_bytes,
                request.requested_bytes,
            )
            .then(
                || Error::FrameAllocationRefusedPastTheDeviceMemoryHeapBudget {
                    heap_index: heap.heap_index,
                    heap_locality: heap.heap_locality,
                    heap_budget_bytes: heap.heap_budget_bytes,
                    heap_usage_bytes: heap.heap_usage_bytes,
                    bytes_this_runtimes_allocator_pools_hold_on_the_heap: heap
                        .bytes_this_runtimes_allocator_pools_hold_on_the_heap,
                    requested_bytes: request.requested_bytes,
                    headroom_bytes:
                        HEADROOM_BYTES_A_FRAME_REQUEST_MUST_LEAVE_UNDER_THE_DEVICE_MEMORY_BUDGET,
                    heap_budget_source: heap.heap_budget_source,
                    frame_allocation_site: request.frame_allocation_site.to_string(),
                },
            )
        }
        #[cfg(any(target_os = "macos", test))]
        FrameAllocationBudgetReading::MetalWorkingSet(working_set) => {
            request_leaves_less_than_the_headroom(
                working_set.recommended_max_working_set_bytes,
                working_set.metal_current_allocated_bytes,
                request.requested_bytes,
            )
            .then(
                || Error::FrameAllocationRefusedPastTheMetalWorkingSetBudget {
                    recommended_max_working_set_bytes: working_set
                        .recommended_max_working_set_bytes,
                    metal_current_allocated_bytes: working_set.metal_current_allocated_bytes,
                    requested_bytes: request.requested_bytes,
                    headroom_bytes:
                        HEADROOM_BYTES_A_FRAME_REQUEST_MUST_LEAVE_UNDER_THE_DEVICE_MEMORY_BUDGET,
                    frame_allocation_site: request.frame_allocation_site.to_string(),
                },
            )
        }
    };
    budget_refusal.or_else(|| {
        let device_memory_allocation_count = reading.allocation_count;
        let allocations_left_under_the_device_limit =
            u64::from(device_memory_allocation_count.max_memory_allocation_count)
                .saturating_sub(device_memory_allocation_count.live_device_memory_allocation_count);
        (allocations_left_under_the_device_limit
            < u64::from(DEVICE_MEMORY_ALLOCATIONS_KEPT_IN_RESERVE_UNDER_THE_DEVICE_LIMIT))
        .then(
            || Error::FrameAllocationRefusedNearTheDeviceMemoryAllocationCountLimit {
                live_device_memory_allocation_count: device_memory_allocation_count
                    .live_device_memory_allocation_count,
                max_memory_allocation_count: device_memory_allocation_count
                    .max_memory_allocation_count,
                allocations_kept_in_reserve:
                    DEVICE_MEMORY_ALLOCATIONS_KEPT_IN_RESERVE_UNDER_THE_DEVICE_LIMIT,
                frame_allocation_site: request.frame_allocation_site.to_string(),
            },
        )
    })
}

/// A heap's budget where the device lacks `VK_EXT_memory_budget`: the
/// allocator's 80% estimate, and for a heap that is not device-local no more
/// than its usage plus the system's available memory, so the room never
/// outruns what the kernel can still hand out.
#[cfg(any(target_os = "linux", test))]
pub(crate) fn heap_budget_without_the_memory_budget_extension(
    allocator_estimate_of_eighty_percent_of_the_heap_bytes: u64,
    heap_usage_bytes: u64,
    heap_locality: DeviceMemoryHeapLocality,
    system_memory_available_bytes: Option<u64>,
) -> (u64, DeviceMemoryHeapBudgetSource) {
    let allocator_estimate_and_its_source = (
        allocator_estimate_of_eighty_percent_of_the_heap_bytes,
        DeviceMemoryHeapBudgetSource::EstimatedAsEightyPercentOfTheHeap,
    );
    match (heap_locality, system_memory_available_bytes) {
        (DeviceMemoryHeapLocality::NotDeviceLocal, Some(system_memory_available_bytes)) => {
            let capped_by_the_systems_available_memory =
                heap_usage_bytes.saturating_add(system_memory_available_bytes);
            if capped_by_the_systems_available_memory
                < allocator_estimate_of_eighty_percent_of_the_heap_bytes
            {
                (
                    capped_by_the_systems_available_memory,
                    DeviceMemoryHeapBudgetSource::EstimatedAndCappedByTheSystemsAvailableMemory,
                )
            } else {
                allocator_estimate_and_its_source
            }
        }
        _ => allocator_estimate_and_its_source,
    }
}

/// `VkDeviceMemory` objects this process holds: the allocator's blocks — each
/// dedicated allocation is one — plus the raw `vkAllocateMemory` calls it
/// made itself. Never VMA's `allocationCount`, which counts sub-allocations.
pub(crate) fn device_memory_allocations_this_process_holds(
    allocator_block_count_per_heap: impl IntoIterator<Item = u32>,
    raw_vk_allocate_memory_count: usize,
) -> u64 {
    allocator_block_count_per_heap
        .into_iter()
        .map(u64::from)
        .sum::<u64>()
        .saturating_add(raw_vk_allocate_memory_count as u64)
}

/// Runs every frame allocation's budget check and its allocation as one step
/// under one device-wide lock, so two allocations never pass on one reading.
pub(crate) struct FrameAllocationDeviceMemoryBudgetGate {
    /// A leaf lock: nothing run under it takes another engine lock.
    check_and_allocate_as_one_step: Mutex<()>,
    #[cfg(test)]
    every_budget_left_with_no_room_by_a_test: std::sync::atomic::AtomicBool,
}

impl FrameAllocationDeviceMemoryBudgetGate {
    /// A gate no allocation has passed through yet.
    pub(crate) fn new() -> Self {
        Self {
            check_and_allocate_as_one_step: Mutex::new(()),
            #[cfg(test)]
            every_budget_left_with_no_room_by_a_test: std::sync::atomic::AtomicBool::new(false),
        }
    }

    /// Allocate a frame only if it fits the device's memory bound on a reading
    /// fetched for this allocation, refusing by name otherwise.
    #[tracing::instrument(
        level = "trace",
        skip_all,
        fields(
            frame_allocation_site = request.frame_allocation_site,
            requested_bytes = request.requested_bytes,
        )
    )]
    pub(crate) fn allocate_a_frame_within_the_device_memory_budget<AllocatedFrame>(
        &self,
        request: &FrameDeviceMemoryRequest,
        read_the_device_memory_fetched_from_the_driver_now: impl FnOnce() -> Result<
            FrameAllocationDeviceMemoryReading,
        >,
        allocate_the_frame: impl FnOnce() -> Result<AllocatedFrame>,
    ) -> Result<AllocatedFrame> {
        let _check_and_allocate_as_one_step = self
            .check_and_allocate_as_one_step
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let reading = read_the_device_memory_fetched_from_the_driver_now()?;
        #[cfg(test)]
        let reading = if self
            .every_budget_left_with_no_room_by_a_test
            .load(std::sync::atomic::Ordering::SeqCst)
        {
            reading.with_no_room_left_in_its_budget()
        } else {
            reading
        };
        if let Some(refusal) =
            refusal_of_a_frame_allocation_past_the_device_memory_bound(request, &reading)
        {
            tracing::debug!(
                frame_allocation_site = request.frame_allocation_site,
                requested_bytes = request.requested_bytes,
                placement = %request.placement,
                refusal = %refusal,
                "frame allocation refused before allocating"
            );
            return Err(refusal);
        }
        allocate_the_frame()
    }

    /// From now on, weigh every frame allocation on this gate against a budget
    /// lowered to its usage.
    #[cfg(test)]
    pub(crate) fn leave_no_room_in_any_budget_for_this_test(&self) {
        self.every_budget_left_with_no_room_by_a_test
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests;
