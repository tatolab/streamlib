// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! A pixel-buffer pool slot as a viewer holds it: the memory's handles and
//! layout, handed on to whatever displays the frame.

use std::os::fd::{AsFd as _, BorrowedFd};
use std::sync::{Arc, OnceLock};

use streamlib_consumer_rhi::{ConsumerVulkanBuffer, ConsumerVulkanDevice};
use streamlib_surface_client::{
    CheckedOutPixelBuffer, CheckedOutPixelBufferMemory, SurfaceShareCheckOutFds,
    resource_type_of_a_check_out,
};

use super::SurfaceShareTransferredHandle;

/// One pixel-buffer pool slot's memory, as the surface-sharing service handed
/// it to a viewer: shared by every frame over the slot.
pub struct PixelBufferPoolSlotOfAViewer {
    checked_out_pixel_buffer: CheckedOutPixelBuffer,
    /// The CPU's view of an OPAQUE_FD allocation, imported on its first read.
    cpu_readable_import: OnceLock<ConsumerVulkanBuffer>,
}

impl PixelBufferPoolSlotOfAViewer {
    /// Read the service's answer to an `import_pool_slot` of `pool_slot_key`
    /// and the descriptors it carried.
    pub(crate) fn from_the_import_reply(
        pool_slot_key: &str,
        reply: &serde_json::Value,
        received_handles: Vec<SurfaceShareTransferredHandle>,
    ) -> Result<Self, String> {
        let resource_type = resource_type_of_a_check_out(reply);
        if resource_type != "pixel_buffer" {
            return Err(format!(
                "pool slot {pool_slot_key:?} is registered as a {resource_type:?}; a viewer \
                 imports pixel buffers only"
            ));
        }
        let SurfaceShareCheckOutFds { plane_fds, .. } =
            SurfaceShareCheckOutFds::split_off_the_timeline_fds(
                pool_slot_key,
                reply,
                received_handles,
            )?;
        Ok(Self {
            checked_out_pixel_buffer: CheckedOutPixelBuffer::from_check_out(
                pool_slot_key,
                reply,
                plane_fds,
            )?,
            cpu_readable_import: OnceLock::new(),
        })
    }

    /// The slot's width in pixels.
    pub fn width(&self) -> u32 {
        self.checked_out_pixel_buffer.width
    }

    /// The slot's height in pixels.
    pub fn height(&self) -> u32 {
        self.checked_out_pixel_buffer.height
    }

    /// The wire name of the slot's pixel format (`"rgba32"`, `"bgra32"`, …).
    pub fn format_wire_name(&self) -> &str {
        &self.checked_out_pixel_buffer.format_wire_name
    }

    /// The first plane's row pitch, padding included.
    pub fn bytes_per_row(&self) -> u64 {
        self.checked_out_pixel_buffer.bytes_per_row
    }

    /// Each plane's byte size, first plane first.
    pub fn plane_sizes(&self) -> &[u64] {
        &self.checked_out_pixel_buffer.plane_sizes
    }

    /// The DMA-BUF descriptor of each plane, first plane first, for a display
    /// that imports them itself; `None` when the memory crossed as OPAQUE_FD,
    /// which only [`Self::bytes_for_a_cpu_read`] reads.
    pub fn dma_buf_plane_fds(&self) -> Option<Vec<BorrowedFd<'_>>> {
        match &self.checked_out_pixel_buffer.memory {
            CheckedOutPixelBufferMemory::DmaBuf { plane_fds } => {
                Some(plane_fds.iter().map(|plane_fd| plane_fd.as_fd()).collect())
            }
            CheckedOutPixelBufferMemory::OpaqueFd { .. } => None,
        }
    }

    /// The UUID of the device that exported memory which crossed as
    /// OPAQUE_FD, which a CPU read imports on; `None` for DMA-BUF planes.
    pub fn opaque_fd_exporting_device_uuid(&self) -> Option<[u8; 16]> {
        match &self.checked_out_pixel_buffer.memory {
            CheckedOutPixelBufferMemory::OpaqueFd {
                exporting_device_uuid,
                ..
            } => Some(*exporting_device_uuid),
            CheckedOutPixelBufferMemory::DmaBuf { .. } => None,
        }
    }

    /// The slot's first plane as the CPU reads it, through an import made at
    /// the first read on `consumer_vulkan_device` — the device
    /// [`Self::opaque_fd_exporting_device_uuid`] names, which every later read
    /// reuses: the fallback for memory that crossed as OPAQUE_FD, which a
    /// display cannot import.
    pub fn bytes_for_a_cpu_read(
        &self,
        consumer_vulkan_device: &Arc<ConsumerVulkanDevice>,
    ) -> Result<&[u8], String> {
        let CheckedOutPixelBufferMemory::OpaqueFd {
            memory_fd,
            vk_memory_type_index,
            ..
        } = &self.checked_out_pixel_buffer.memory
        else {
            return Err(
                "a DMA-BUF pool slot is read by importing its planes, not through a CPU \
                 import"
                    .into(),
            );
        };
        let plane0_size = self
            .plane_sizes()
            .first()
            .copied()
            .filter(|&plane0_size| plane0_size > 0)
            .ok_or("the pool slot's import stated no first plane size")?;
        if let Some(imported) = self.cpu_readable_import.get() {
            return mapped_bytes(imported, plane0_size);
        }
        let duplicated_for_import = memory_fd.try_clone().map_err(|duplicate_failure| {
            format!("could not duplicate the pool slot's memory fd for import: {duplicate_failure}")
        })?;
        let imported = ConsumerVulkanBuffer::from_opaque_fd_at_stated_memory_type_index(
            consumer_vulkan_device,
            duplicated_for_import,
            plane0_size,
            *vk_memory_type_index,
        )
        .map_err(|import_failure| {
            format!(
                "Vulkan could not import the pool slot's OPAQUE_FD memory at memory type index \
                 {vk_memory_type_index}: {import_failure}"
            )
        })?;
        mapped_bytes(
            self.cpu_readable_import.get_or_init(|| imported),
            plane0_size,
        )
    }
}

fn mapped_bytes(imported: &ConsumerVulkanBuffer, plane0_size: u64) -> Result<&[u8], String> {
    let mapped = imported.mapped_ptr();
    if mapped.is_null() {
        return Err("the pool slot's memory imported without a host mapping".into());
    }
    // SAFETY: the import maps `plane0_size` bytes for as long as it lives,
    // and it lives as long as the slot that holds it.
    Ok(unsafe { std::slice::from_raw_parts(mapped, plane0_size as usize) })
}
