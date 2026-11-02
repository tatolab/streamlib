// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Linear NV12 buffers whose memory is exported as DMA-BUFs, rotated so the
//! picture handed out stays intact while the next one decodes into another.

use std::os::fd::{FromRawFd as _, OwnedFd};
use std::sync::Arc;

use vulkanalia::prelude::v1_4::*;
use vulkanalia::vk;
use vulkanalia::vk::KhrExternalMemoryFdExtensionDeviceCommands as _;

use crate::VideoError;

/// Buffers a ring starts with: one being read by its consumer, one decoding,
/// and one spare so a consumer that lags a picture behind never sees a write.
/// The ring grows past this when every buffer is still held by a consumer.
pub(crate) const EXPORTED_NV12_DMA_BUF_RING_DEPTH: usize = 3;

/// A decoded picture's NV12 planes in a DMA-BUF this decoder exported: the Y
/// plane then the interleaved CbCr plane, linear, each at its offset and row
/// stride inside `allocation_byte_size` bytes.
#[derive(Debug, Clone)]
pub struct ExportedNv12DmaBufPicture {
    /// The exported memory; it stays this decoder's, valid while any clone
    /// of it is held.
    pub dma_buf_fd: Arc<OwnedFd>,
    /// Which of the ring's buffers this is, stable until `ring_generation`
    /// changes — a consumer may adopt each buffer once and reuse it. The
    /// ring never writes a buffer while any clone of its `dma_buf_fd` is
    /// held outside it.
    pub ring_slot_index: usize,
    /// Bumped whenever the ring is reallocated, for a new coded extent.
    pub ring_generation: u64,
    pub allocation_byte_size: u64,
    pub plane_offsets_bytes: [u64; 2],
    pub plane_strides_bytes: [u64; 2],
}

struct ExportedNv12DmaBufSlot {
    buffer: vk::Buffer,
    memory: vk::DeviceMemory,
    dma_buf_fd: Arc<OwnedFd>,
    allocation_byte_size: u64,
}

impl ExportedNv12DmaBufSlot {
    fn is_held_by_a_consumer(&self) -> bool {
        Arc::strong_count(&self.dma_buf_fd) > 1
    }
}

/// The ring of exported buffers a decoder copies each picture into.
pub(crate) struct ExportedNv12DmaBufRing {
    device: vulkanalia::Device,
    slots: Vec<ExportedNv12DmaBufSlot>,
    /// Buffers of an earlier coded extent a consumer still holds, freed once
    /// it lets go.
    retired_slots_held_by_a_consumer: Vec<ExportedNv12DmaBufSlot>,
    next_slot_index: usize,
    coded_width: u32,
    coded_height: u32,
    generation: u64,
}

impl ExportedNv12DmaBufRing {
    /// An empty ring on `device`; buffers are allocated at the first picture.
    pub(crate) fn new(device: vulkanalia::Device) -> Self {
        Self {
            device,
            slots: Vec::new(),
            retired_slots_held_by_a_consumer: Vec::new(),
            next_slot_index: 0,
            coded_width: 0,
            coded_height: 0,
            generation: 0,
        }
    }

    /// The buffer the next picture is copied into and its slot index: the
    /// next buffer no consumer holds, after reallocating the ring when the
    /// coded extent changed, or a new buffer when a consumer holds them all.
    ///
    /// # Safety
    ///
    /// No decode may still be writing any of the ring's buffers when the
    /// extent changes.
    pub(crate) unsafe fn next_buffer_for_a_picture(
        &mut self,
        instance: &vulkanalia::Instance,
        physical_device: vk::PhysicalDevice,
        coded_width: u32,
        coded_height: u32,
    ) -> Result<(vk::Buffer, usize), VideoError> {
        unsafe { self.free_retired_slots_no_consumer_holds() };
        let picture_byte_size = nv12_picture_byte_size(coded_width, coded_height);
        if self.slots.is_empty()
            || (self.coded_width, self.coded_height) != (coded_width, coded_height)
        {
            unsafe { self.retire_slots() };
            self.coded_width = coded_width;
            self.coded_height = coded_height;
            self.generation += 1;
            self.next_slot_index = 0;
            for _ in 0..EXPORTED_NV12_DMA_BUF_RING_DEPTH {
                if let Err(allocation_failure) = unsafe {
                    self.allocate_one_more_slot(instance, physical_device, picture_byte_size)
                } {
                    unsafe { self.retire_slots() };
                    return Err(allocation_failure);
                }
            }
        }
        let slot_count = self.slots.len();
        let free_slot_index = (0..slot_count)
            .map(|offset| (self.next_slot_index + offset) % slot_count)
            .find(|&slot_index| !self.slots[slot_index].is_held_by_a_consumer());
        let slot_index = match free_slot_index {
            Some(slot_index) => slot_index,
            None => {
                unsafe {
                    self.allocate_one_more_slot(instance, physical_device, picture_byte_size)?
                };
                slot_count
            }
        };
        self.next_slot_index = (slot_index + 1) % self.slots.len();
        Ok((self.slots[slot_index].buffer, slot_index))
    }

    unsafe fn allocate_one_more_slot(
        &mut self,
        instance: &vulkanalia::Instance,
        physical_device: vk::PhysicalDevice,
        picture_byte_size: u64,
    ) -> Result<(), VideoError> {
        let slot = unsafe {
            allocate_exported_slot(&self.device, instance, physical_device, picture_byte_size)?
        };
        self.slots.push(slot);
        Ok(())
    }

    /// The picture slot `slot_index` holds, described for a consumer: the
    /// display window's planes inside the coded picture copied there.
    pub(crate) fn picture_in_slot(
        &self,
        slot_index: usize,
        display_origin_x: u32,
        display_origin_y: u32,
    ) -> Option<ExportedNv12DmaBufPicture> {
        let slot = self.slots.get(slot_index)?;
        let coded_width = u64::from(self.coded_width);
        let luma_plane_byte_size = coded_width * u64::from(self.coded_height);
        let origin_x = u64::from(display_origin_x & !1);
        let origin_y = u64::from(display_origin_y & !1);
        Some(ExportedNv12DmaBufPicture {
            dma_buf_fd: Arc::clone(&slot.dma_buf_fd),
            ring_slot_index: slot_index,
            ring_generation: self.generation,
            allocation_byte_size: slot.allocation_byte_size,
            plane_offsets_bytes: [
                origin_y * coded_width + origin_x,
                luma_plane_byte_size + (origin_y / 2) * coded_width + origin_x,
            ],
            plane_strides_bytes: [coded_width, coded_width],
        })
    }

    /// Move every buffer out of the ring, freeing those no consumer holds.
    unsafe fn retire_slots(&mut self) {
        let slots = std::mem::take(&mut self.slots);
        self.retired_slots_held_by_a_consumer.extend(slots);
        unsafe { self.free_retired_slots_no_consumer_holds() };
    }

    unsafe fn free_retired_slots_no_consumer_holds(&mut self) {
        let (still_held, released): (Vec<_>, Vec<_>) = self
            .retired_slots_held_by_a_consumer
            .drain(..)
            .partition(ExportedNv12DmaBufSlot::is_held_by_a_consumer);
        self.retired_slots_held_by_a_consumer = still_held;
        for slot in released {
            unsafe { destroy_slot(&self.device, slot) };
        }
    }
}

unsafe fn destroy_slot(device: &vulkanalia::Device, slot: ExportedNv12DmaBufSlot) {
    unsafe {
        device.destroy_buffer(slot.buffer, None);
        device.free_memory(slot.memory, None);
    }
}

impl Drop for ExportedNv12DmaBufRing {
    fn drop(&mut self) {
        let all_slots = self
            .slots
            .drain(..)
            .chain(self.retired_slots_held_by_a_consumer.drain(..));
        for slot in all_slots.collect::<Vec<_>>() {
            // SAFETY: the owning decoder waits for its device to idle first. A
            // consumer still holding an fd keeps the DMA-BUF's memory alive.
            unsafe { destroy_slot(&self.device, slot) };
        }
    }
}

/// The bytes a tightly packed NV12 picture of the coded extent fills: the
/// copy the decode records writes the Y plane then the CbCr plane at the
/// coded width.
fn nv12_picture_byte_size(coded_width: u32, coded_height: u32) -> u64 {
    let luma_plane_byte_size = u64::from(coded_width) * u64::from(coded_height);
    luma_plane_byte_size + luma_plane_byte_size / 2
}

/// Allocate one buffer on dedicated, host-visible memory exported as a
/// DMA-BUF. Host-visible so an importer that cannot import the DMA-BUF into
/// its own device can still map and read it.
unsafe fn allocate_exported_slot(
    device: &vulkanalia::Device,
    instance: &vulkanalia::Instance,
    physical_device: vk::PhysicalDevice,
    picture_byte_size: u64,
) -> Result<ExportedNv12DmaBufSlot, VideoError> {
    let mut external_memory_buffer_info = vk::ExternalMemoryBufferCreateInfo::builder()
        .handle_types(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT);
    let buffer_info = vk::BufferCreateInfo::builder()
        .size(picture_byte_size)
        .usage(vk::BufferUsageFlags::TRANSFER_DST)
        .sharing_mode(vk::SharingMode::EXCLUSIVE)
        .push_next(&mut external_memory_buffer_info);
    let buffer = unsafe { device.create_buffer(&buffer_info, None) }.map_err(VideoError::from)?;
    let memory_requirements = unsafe { device.get_buffer_memory_requirements(buffer) };
    let memory_properties =
        unsafe { instance.get_physical_device_memory_properties(physical_device) };
    let host_visible_and_coherent =
        vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT;
    let Some(memory_type_index) = (0..memory_properties.memory_type_count).find(|&index| {
        memory_requirements.memory_type_bits & (1 << index) != 0
            && memory_properties.memory_types[index as usize]
                .property_flags
                .contains(host_visible_and_coherent)
    }) else {
        unsafe { device.destroy_buffer(buffer, None) };
        return Err(VideoError::BitstreamError(
            "no host-visible, host-coherent memory type can back an exported NV12 picture".into(),
        ));
    };
    let mut export_memory_info = vk::ExportMemoryAllocateInfo::builder()
        .handle_types(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT);
    let mut dedicated_allocation_info = vk::MemoryDedicatedAllocateInfo::builder().buffer(buffer);
    let allocate_info = vk::MemoryAllocateInfo::builder()
        .allocation_size(memory_requirements.size)
        .memory_type_index(memory_type_index)
        .push_next(&mut export_memory_info)
        .push_next(&mut dedicated_allocation_info);
    let memory = match unsafe { device.allocate_memory(&allocate_info, None) } {
        Ok(memory) => memory,
        Err(allocation_failure) => {
            unsafe { device.destroy_buffer(buffer, None) };
            return Err(VideoError::from(allocation_failure));
        }
    };
    let exported = unsafe { device.bind_buffer_memory(buffer, memory, 0) }.and_then(|()| {
        let get_fd_info = vk::MemoryGetFdInfoKHR::builder()
            .memory(memory)
            .handle_type(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT);
        unsafe { device.get_memory_fd_khr(&get_fd_info) }
    });
    let raw_dma_buf_fd = match exported {
        Ok(raw_dma_buf_fd) => raw_dma_buf_fd,
        Err(export_failure) => {
            unsafe {
                device.destroy_buffer(buffer, None);
                device.free_memory(memory, None);
            }
            return Err(VideoError::BitstreamError(format!(
                "an NV12 picture's memory could not be exported as a DMA-BUF: {export_failure}"
            )));
        }
    };
    Ok(ExportedNv12DmaBufSlot {
        buffer,
        memory,
        // SAFETY: vkGetMemoryFdKHR hands the caller a new fd it alone owns.
        dma_buf_fd: Arc::new(unsafe { OwnedFd::from_raw_fd(raw_dma_buf_fd) }),
        allocation_byte_size: memory_requirements.size,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_nv12_picture_fills_one_and_a_half_luma_planes() {
        assert_eq!(nv12_picture_byte_size(320, 192), 320 * 192 * 3 / 2);
    }
}
