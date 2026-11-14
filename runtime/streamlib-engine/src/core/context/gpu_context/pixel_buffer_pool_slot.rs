// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use crate::core::context::gpu_pool_claim_table::GpuPoolClaimTableRowLease;
use crate::core::context::lease_aware_pool_slot_ring::{
    LeaseAwarePoolSlot, LeaseAwarePoolSlotResource,
};
use crate::core::context::shared_pool_entry_freeing::SharedPoolEntryFreeingRecord;
use crate::core::rhi::{
    PixelBuffer, PixelBufferPoolSlotId, PixelFormat, PublishedPixelBufferFrameId,
};
use crate::core::runtime::LoadedStreamTag;

/// The shape a shared pixel-buffer ring holds buffers of.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(in crate::core::context) struct PixelBufferPoolKey {
    pub(in crate::core::context) width: u32,
    pub(in crate::core::context) height: u32,
    pub(in crate::core::context) format: PixelFormat,
}

/// One pixel-buffer pool slot's resource, as the lease-aware ring sees it.
pub(in crate::core::context) struct PixelBufferPoolSlot {
    pub(in crate::core::context) buffer: PixelBuffer,
    pub(in crate::core::context) freeing_record: SharedPoolEntryFreeingRecord,
    gpu_pool_claim_table_row: Option<GpuPoolClaimTableRowLease>,
}

impl PixelBufferPoolSlot {
    pub(super) fn allocated_for(
        buffer: PixelBuffer,
        acquiring_stream: Option<LoadedStreamTag>,
        gpu_pool_claim_table_row: Option<GpuPoolClaimTableRowLease>,
    ) -> Self {
        Self {
            buffer,
            freeing_record: SharedPoolEntryFreeingRecord::handed_out_to(acquiring_stream),
            gpu_pool_claim_table_row,
        }
    }
}

impl LeaseAwarePoolSlotResource for PixelBufferPoolSlot {
    /// `PixelBuffer` holds an opaque handle to a host-side `Arc`; the
    /// baseline is 2 — one share in the ring that holds the slot, the shared
    /// ring or the native node output pool it is lent to, and one under the
    /// current published id in `buffer_cache` (1 before the slot ever
    /// publishes, when no consumer can hold it either) — so anything above
    /// that is a live reader.
    fn is_held_in_this_process(&self) -> bool {
        self.buffer.strong_count() > 2
    }

    fn gpu_pool_claim_table_row(&self) -> Option<&GpuPoolClaimTableRowLease> {
        self.gpu_pool_claim_table_row.as_ref()
    }

    fn is_flagged_freed_as_soon_as_unheld(&self) -> bool {
        self.freeing_record.is_flagged_freed_as_soon_as_unheld()
    }

    /// Whether the kernel reports the slot's IOSurface in use by any
    /// process: a Mach port to it still in flight, or a use count a helper
    /// holds. Kernel-truthful, and cleared by the kernel when that process
    /// dies, so it holds a slot even where no checkout lease was taken.
    #[cfg(target_os = "macos")]
    fn is_in_use_per_the_platform(&self) -> bool {
        self.buffer
            .buffer_ref()
            .inner
            .backing_iosurface()
            .is_some_and(objc2_io_surface::IOSurfaceRef::is_in_use)
    }
}

/// The published frame id a pixel-buffer slot's most recent hand-off minted.
pub(in crate::core::context) fn published_pixel_buffer_frame_id(
    slot: &LeaseAwarePoolSlot<PixelBufferPoolSlot>,
) -> PublishedPixelBufferFrameId {
    PublishedPixelBufferFrameId::new(
        PixelBufferPoolSlotId::from_str(slot.pool_slot_key()),
        slot.published_frame_generation(),
    )
}

/// The descriptor a reader claims a pixel-buffer slot's most recent frame by,
/// or `None` when the slot leases no claim-table row.
pub(in crate::core::context) fn gpu_pool_slot_frame_of_the_pixel_buffer_slot(
    slot: &LeaseAwarePoolSlot<PixelBufferPoolSlot>,
) -> Option<streamlib_ipc_types::GpuPoolSlotFrameDescriptorBody> {
    slot.resource()
        .gpu_pool_claim_table_row
        .as_ref()
        .map(
            |claim_table_row| streamlib_ipc_types::GpuPoolSlotFrameDescriptorBody {
                pool: claim_table_row.table().pool_uuid(),
                slot: claim_table_row.row_index(),
                generation: slot.published_frame_generation(),
                timeline_value: 0,
                image_layout: 0,
            },
        )
}
