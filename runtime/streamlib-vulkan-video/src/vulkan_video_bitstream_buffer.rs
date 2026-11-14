// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A codec's bitstream buffer, bound to its video profile and persistently
//! mapped.

use std::sync::Arc;

use vulkanalia::prelude::v1_4::*;
use vulkanalia::vk;
use vulkanalia_vma::{self as vma, Alloc as _};

use crate::{VideoError, VideoResult, VulkanVideoCodecDevice};

/// Whether the driver writes the buffer (encode) or reads it (decode).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoBitstreamDirection {
    /// The driver writes compressed NAL bytes; the CPU reads them back.
    Encode,
    /// The CPU writes compressed NAL bytes; the driver reads them.
    Decode,
}

/// Inputs for [`VulkanVideoBitstreamBuffer::new`]. A frame that outgrows
/// `size` is the codec's to handle, by dropping the buffer and minting a
/// larger one.
pub struct VideoBitstreamBufferDescriptor<'a> {
    pub label: &'a str,
    pub size: u64,
    pub direction: VideoBitstreamDirection,
    pub video_profile: &'a vk::VideoProfileInfoKHR,
}

/// A HOST_VISIBLE, HOST_COHERENT, persistently mapped bitstream `VkBuffer`,
/// freed on drop.
pub struct VulkanVideoBitstreamBuffer {
    codec_device: Arc<dyn VulkanVideoCodecDevice>,
    buffer: vk::Buffer,
    allocation: vma::Allocation,
    mapped_ptr: *mut u8,
    size: vk::DeviceSize,
}

// SAFETY: the buffer is only destroyed on drop; the mapping is written and
// read by the codec that owns the value, through `&mut` call paths.
unsafe impl Send for VulkanVideoBitstreamBuffer {}
unsafe impl Sync for VulkanVideoBitstreamBuffer {}

impl VulkanVideoBitstreamBuffer {
    /// Allocate the buffer under the device's resource lock.
    pub fn new(
        codec_device: &Arc<dyn VulkanVideoCodecDevice>,
        descriptor: &VideoBitstreamBufferDescriptor<'_>,
    ) -> VideoResult<Self> {
        if descriptor.size == 0 {
            return Err(VideoError::Configuration(format!(
                "VulkanVideoBitstreamBuffer::new ({}): size must be > 0",
                descriptor.label,
            )));
        }
        let usage = match descriptor.direction {
            VideoBitstreamDirection::Encode => vk::BufferUsageFlags::VIDEO_ENCODE_DST_KHR,
            VideoBitstreamDirection::Decode => vk::BufferUsageFlags::VIDEO_DECODE_SRC_KHR,
        };
        let mut profile_list = vk::VideoProfileListInfoKHR::builder()
            .profiles(std::slice::from_ref(descriptor.video_profile));
        let buffer_info = vk::BufferCreateInfo::builder()
            .size(descriptor.size)
            .usage(usage)
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .push_next(&mut profile_list);
        let alloc_options = vma::AllocationOptions {
            flags: vma::AllocationCreateFlags::MAPPED
                | vma::AllocationCreateFlags::HOST_ACCESS_SEQUENTIAL_WRITE,
            required_flags: vk::MemoryPropertyFlags::HOST_VISIBLE
                | vk::MemoryPropertyFlags::HOST_COHERENT,
            ..Default::default()
        };
        let allocator = codec_device.allocator();
        let _device_lock = codec_device.lock_device();
        let (buffer, allocation) = unsafe { allocator.create_buffer(buffer_info, &alloc_options) }
            .map_err(|e| {
                VideoError::Rhi(format!(
                    "VulkanVideoBitstreamBuffer::new ({}): vmaCreateBuffer failed: {e}",
                    descriptor.label,
                ))
            })?;
        let mapped_ptr = allocator
            .get_allocation_info(allocation)
            .pMappedData
            .cast::<u8>();
        if mapped_ptr.is_null() {
            unsafe { allocator.destroy_buffer(buffer, allocation) };
            return Err(VideoError::Rhi(format!(
                "VulkanVideoBitstreamBuffer::new ({}): VMA mapped pointer is null — expected \
                 persistent mapping",
                descriptor.label,
            )));
        }
        Ok(Self {
            codec_device: Arc::clone(codec_device),
            buffer,
            allocation,
            mapped_ptr,
            size: descriptor.size,
        })
    }

    /// The `VkBuffer`, valid until this value drops.
    pub fn buffer(&self) -> vk::Buffer {
        self.buffer
    }

    /// The persistent CPU mapping of the whole buffer.
    pub fn mapped_ptr(&self) -> *mut u8 {
        self.mapped_ptr
    }

    /// The buffer's size in bytes.
    pub fn size(&self) -> vk::DeviceSize {
        self.size
    }
}

impl Drop for VulkanVideoBitstreamBuffer {
    fn drop(&mut self) {
        unsafe {
            self.codec_device
                .allocator()
                .destroy_buffer(self.buffer, self.allocation)
        };
    }
}
