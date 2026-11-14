// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: GPL-3.0-only

//! A DMA-BUF mapped read-only into this process, for a reader that reaches
//! a foreign device's pixels through the CPU's view of them rather than a
//! Vulkan import of the buffer.

use std::os::fd::{AsRawFd as _, OwnedFd};

use crate::core::{Error, Result};

/// `DMA_BUF_IOCTL_SYNC`: `_IOW('b', 0, struct dma_buf_sync)`.
const DMA_BUF_IOCTL_SYNC: libc::c_ulong = 0x4008_6200;
const DMA_BUF_SYNC_READ: u64 = 1;
const DMA_BUF_SYNC_START: u64 = 0;
const DMA_BUF_SYNC_END: u64 = 4;

/// `struct dma_buf_sync` from `<linux/dma-buf.h>`.
#[repr(C)]
struct DmaBufSync {
    flags: u64,
}

/// A read-only shared mapping of a whole DMA-BUF, unmapped on drop.
///
/// The mapping spans the allocation rounded up to whole pages, so its start
/// and length keep the page alignment a host-pointer import asks for.
pub(crate) struct DmaBufMappedForCpuReads {
    dma_buf_fd: OwnedFd,
    mapped_ptr: *mut libc::c_void,
    mapped_byte_len: usize,
    allocation_byte_size: usize,
}

// SAFETY: the mapping is read-only and owned by this value alone; moving it
// to another thread moves its only owner with it.
unsafe impl Send for DmaBufMappedForCpuReads {}

impl DmaBufMappedForCpuReads {
    /// Map `dma_buf_fd`'s first `allocation_byte_size` bytes, rounded up to
    /// whole pages, read-only.
    pub(crate) fn map(dma_buf_fd: OwnedFd, allocation_byte_size: u64) -> Result<Self> {
        let allocation_byte_size = usize::try_from(allocation_byte_size)
            .ok()
            .filter(|&size| size > 0)
            .ok_or_else(|| {
                Error::Configuration(format!(
                    "a DMA-BUF of {allocation_byte_size} bytes cannot be mapped"
                ))
            })?;
        // SAFETY: sysconf has no preconditions.
        let page_byte_size = usize::try_from(unsafe { libc::sysconf(libc::_SC_PAGESIZE) })
            .unwrap_or(4096)
            .max(1);
        let mapped_byte_len = allocation_byte_size.next_multiple_of(page_byte_size);
        // SAFETY: a fresh shared read-only mapping of an fd this value owns;
        // the kernel picks the address.
        let mapped_ptr = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                mapped_byte_len,
                libc::PROT_READ,
                libc::MAP_SHARED,
                dma_buf_fd.as_raw_fd(),
                0,
            )
        };
        if mapped_ptr == libc::MAP_FAILED {
            return Err(Error::GpuError(format!(
                "mapping the {allocation_byte_size}-byte DMA-BUF for reads failed: {}",
                std::io::Error::last_os_error()
            )));
        }
        Ok(Self {
            dma_buf_fd,
            mapped_ptr,
            mapped_byte_len,
            allocation_byte_size,
        })
    }

    /// The first mapped byte.
    pub(crate) fn mapped_ptr(&self) -> *const u8 {
        self.mapped_ptr.cast_const().cast()
    }

    /// The mapping's length: the allocation rounded up to whole pages.
    pub(crate) fn mapped_byte_len(&self) -> usize {
        self.mapped_byte_len
    }

    /// The allocation's own length, the bytes a reader may read.
    pub(crate) fn allocation_byte_size(&self) -> usize {
        self.allocation_byte_size
    }

    /// Tell the exporter a read through the mapping begins, so the bytes the
    /// device wrote are coherent in it.
    pub(crate) fn begin_reads(&self) -> Result<()> {
        self.sync(DMA_BUF_SYNC_START | DMA_BUF_SYNC_READ, "begin")
    }

    /// Tell the exporter the read through the mapping ended.
    pub(crate) fn end_reads(&self) -> Result<()> {
        self.sync(DMA_BUF_SYNC_END | DMA_BUF_SYNC_READ, "end")
    }

    fn sync(&self, flags: u64, phase: &str) -> Result<()> {
        let request = DmaBufSync { flags };
        loop {
            // SAFETY: DMA_BUF_IOCTL_SYNC reads one `struct dma_buf_sync`
            // from the pointer, which outlives the call.
            let answer = unsafe {
                libc::ioctl(
                    self.dma_buf_fd.as_raw_fd(),
                    DMA_BUF_IOCTL_SYNC,
                    &request as *const DmaBufSync,
                )
            };
            if answer == 0 {
                return Ok(());
            }
            let failure = std::io::Error::last_os_error();
            // An fd that answers ENOTTY has no exporter to sync: it is plain
            // shared memory, which its mapping already sees coherently.
            if failure.raw_os_error() == Some(libc::ENOTTY) {
                return Ok(());
            }
            if failure.kind() != std::io::ErrorKind::Interrupted {
                return Err(Error::GpuError(format!(
                    "DMA_BUF_IOCTL_SYNC ({phase} read) failed: {failure}"
                )));
            }
        }
    }
}

impl Drop for DmaBufMappedForCpuReads {
    fn drop(&mut self) {
        // SAFETY: the mapping this value made, unmapped once.
        unsafe { libc::munmap(self.mapped_ptr, self.mapped_byte_len) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::fd::FromRawFd as _;

    /// A memfd stands in for the DMA-BUF: the mapping is the same `mmap`.
    fn memfd_holding(bytes: &[u8]) -> OwnedFd {
        // SAFETY: a fresh memfd this test owns.
        let fd = unsafe { libc::memfd_create(c"dma_buf_stand_in".as_ptr(), 0) };
        assert!(fd >= 0, "memfd_create");
        let owned = unsafe { OwnedFd::from_raw_fd(fd) };
        let written = unsafe { libc::write(fd, bytes.as_ptr().cast(), bytes.len()) };
        assert_eq!(written as usize, bytes.len());
        owned
    }

    #[test]
    fn a_mapping_spans_whole_pages_and_reads_the_allocations_bytes() {
        let allocation: Vec<u8> = (0..5000u32).map(|i| (i % 251) as u8).collect();
        let mapping =
            DmaBufMappedForCpuReads::map(memfd_holding(&allocation), allocation.len() as u64)
                .expect("map");
        let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as usize;
        assert_eq!(mapping.mapped_byte_len() % page, 0);
        assert!(mapping.mapped_byte_len() >= allocation.len());
        assert_eq!(mapping.allocation_byte_size(), allocation.len());
        assert_eq!(mapping.mapped_ptr() as usize % page, 0);
        let mapped = unsafe { std::slice::from_raw_parts(mapping.mapped_ptr(), allocation.len()) };
        assert_eq!(mapped, allocation.as_slice());
    }

    #[test]
    fn an_empty_allocation_is_refused() {
        assert!(DmaBufMappedForCpuReads::map(memfd_holding(&[1]), 0).is_err());
    }

    #[test]
    fn a_sync_on_shared_memory_with_no_exporter_needs_nothing() {
        let mapping = DmaBufMappedForCpuReads::map(memfd_holding(&[7; 64]), 64).expect("map");
        mapping.begin_reads().expect("a memfd has nothing to sync");
        mapping.end_reads().expect("a memfd has nothing to sync");
    }
}
