// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

/// Whether a device memory heap is the device's own memory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceMemoryHeapLocality {
    /// The heap carries `VK_MEMORY_HEAP_DEVICE_LOCAL_BIT`.
    DeviceLocal,
    /// The heap is system memory the device reaches.
    NotDeviceLocal,
}

impl std::fmt::Display for DeviceMemoryHeapLocality {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DeviceLocal => f.write_str("device-local"),
            Self::NotDeviceLocal => f.write_str("not device-local"),
        }
    }
}

/// Where a device memory heap's budget came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceMemoryHeapBudgetSource {
    /// `VK_EXT_memory_budget`'s `heapBudget`, as the driver reported it.
    ReportedByTheDriver,
    /// The allocator's estimate without the extension: 80% of the heap's size.
    EstimatedAsEightyPercentOfTheHeap,
    /// That estimate, lowered to the heap's usage plus the system's available memory.
    EstimatedAndCappedByTheSystemsAvailableMemory,
}

impl std::fmt::Display for DeviceMemoryHeapBudgetSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ReportedByTheDriver => f.write_str("reported by the driver"),
            Self::EstimatedAsEightyPercentOfTheHeap => {
                f.write_str("estimated as 80% of the heap, the driver reporting no budget")
            }
            Self::EstimatedAndCappedByTheSystemsAvailableMemory => f.write_str(
                "estimated as 80% of the heap, the driver reporting no budget, and capped by \
                 the system's available memory",
            ),
        }
    }
}
