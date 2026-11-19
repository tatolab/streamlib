// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The driver's memory-mapped capture buffers, mapped read-only so the MMAP
//! path can copy a dequeued buffer's frame out by the index the driver names.

use std::io;
use std::os::fd::RawFd;

use super::v4l2_capture_buffer_queue::memory_mapped_capture_buffer;

/// One read-only mapping per driver buffer, unmapped on drop. It must drop
/// before the `v4l` stream that requested the buffers, whose release of them
/// is refused while any mapping is left.
pub(super) struct V4l2DriverBuffersMappedForReading {
    mappings: Vec<(*const u8, usize)>,
}

impl V4l2DriverBuffersMappedForReading {
    /// Map each of the device's `buffer_count` capture buffers.
    pub(super) fn map_every_buffer_of(device_fd: RawFd, buffer_count: u32) -> io::Result<Self> {
        let mut mapped = Self {
            mappings: Vec::with_capacity(buffer_count as usize),
        };
        for buffer_index in 0..buffer_count {
            let mut buffer = memory_mapped_capture_buffer();
            buffer.index = buffer_index;
            // SAFETY: `buffer` is a capture buffer descriptor QUERYBUF fills in.
            let queried = unsafe {
                libc::ioctl(
                    device_fd,
                    v4l::v4l2::vidioc::VIDIOC_QUERYBUF as libc::c_ulong,
                    &mut buffer,
                )
            };
            if queried != 0 {
                return Err(io::Error::last_os_error());
            }
            let length = buffer.length as usize;
            // SAFETY: QUERYBUF answered this buffer's offset and length on
            // `device_fd`; the mapping is read-only and shared with the driver.
            let mapping = unsafe {
                libc::mmap(
                    std::ptr::null_mut(),
                    length,
                    libc::PROT_READ,
                    libc::MAP_SHARED,
                    device_fd,
                    buffer.m.offset as libc::off_t,
                )
            };
            if mapping == libc::MAP_FAILED {
                return Err(io::Error::last_os_error());
            }
            mapped
                .mappings
                .push((mapping.cast_const().cast::<u8>(), length));
        }
        Ok(mapped)
    }

    /// The whole buffer at `buffer_index`, which must be dequeued, so the
    /// driver is not writing it.
    pub(super) fn bytes_of_a_dequeued_buffer(&self, buffer_index: u32) -> &[u8] {
        let Some(&(mapping, length)) = self.mappings.get(buffer_index as usize) else {
            return &[];
        };
        // SAFETY: the mapping is live until `self` drops and `length` long.
        unsafe { std::slice::from_raw_parts(mapping, length) }
    }
}

impl Drop for V4l2DriverBuffersMappedForReading {
    fn drop(&mut self) {
        for &(mapping, length) in &self.mappings {
            // SAFETY: each pair is a mapping `map_every_buffer_of` made.
            unsafe {
                libc::munmap(mapping.cast_mut().cast(), length);
            }
        }
    }
}
