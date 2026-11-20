// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The driver's memory-mapped capture buffers, mapped read-only so the MMAP
//! path can copy a held buffer's frame out.

use std::io;
use std::marker::PhantomData;

use super::v4l2_capture_buffer_queue::{
    HeldV4l2Buffer, V4l2DeviceCaptureBufferQueue, query_a_driver_buffer,
};

/// One read-only mapping per driver buffer, unmapped on drop, borrowing the
/// `v4l` stream that requested the buffers so it drops first.
pub(super) struct V4l2DriverBuffersMappedForReading<'stream> {
    mappings: Vec<(*const u8, usize)>,
    stream: PhantomData<&'stream v4l::io::mmap::Stream<'stream>>,
}

impl<'stream> V4l2DriverBuffersMappedForReading<'stream> {
    /// Map each buffer of the queue's device.
    pub(super) fn map_every_buffer_of(
        queue: &V4l2DeviceCaptureBufferQueue<'stream>,
    ) -> io::Result<Self> {
        let device_fd = queue.device_fd();
        let mut mapped = Self {
            mappings: Vec::with_capacity(queue.buffer_count() as usize),
            stream: PhantomData,
        };
        for buffer_index in 0..queue.buffer_count() {
            let buffer = query_a_driver_buffer(device_fd, buffer_index)?;
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

    /// The bytes of a buffer this side holds, which the driver is not
    /// writing; `None` when the driver named a buffer it never granted.
    pub(super) fn bytes_of<'held>(
        &'held self,
        buffer: &'held HeldV4l2Buffer,
    ) -> Option<&'held [u8]> {
        let &(mapping, length) = self.mappings.get(buffer.index() as usize)?;
        // SAFETY: the mapping is live until `self` drops and `length` long,
        // and the borrow of `buffer` keeps it out of the driver's queue.
        Some(unsafe { std::slice::from_raw_parts(mapping, length) })
    }
}

impl Drop for V4l2DriverBuffersMappedForReading<'_> {
    fn drop(&mut self) {
        for &(mapping, length) in &self.mappings {
            // SAFETY: each pair is a mapping `map_every_buffer_of` made.
            unsafe {
                libc::munmap(mapping.cast_mut().cast(), length);
            }
        }
    }
}
