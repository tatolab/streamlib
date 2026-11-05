// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! A GPU pool claim table's rows in a memfd, mapped shared so every
//! interpreter handed the fd claims in the same memory.

use std::os::fd::{AsFd, BorrowedFd, FromRawFd as _, OwnedFd};
use std::ptr::NonNull;

use streamlib_ipc_types::{
    GPU_POOL_CLAIM_TABLE_BYTES, GPU_POOL_CLAIM_TABLE_ROWS, GpuPoolClaimTableRow,
};

use crate::core::{Error, Result};

/// A claim table's rows, in a memfd whose size is sealed so no holder can
/// shrink the mapping under another.
pub struct GpuPoolClaimTableMemfd {
    memfd: OwnedFd,
    rows: NonNull<GpuPoolClaimTableRow>,
}

// SAFETY: the mapping is shared memory of atomics only, read and written
// through `&GpuPoolClaimTableRow`, and unmapped once in `Drop`.
unsafe impl Send for GpuPoolClaimTableMemfd {}
// SAFETY: as above; every access is an atomic operation.
unsafe impl Sync for GpuPoolClaimTableMemfd {}

impl GpuPoolClaimTableMemfd {
    /// A fresh table: zeroed rows, which publish nothing and hold no claims.
    pub fn create(pool_name: &str) -> Result<Self> {
        let memfd_name = std::ffi::CString::new(format!("streamlib-gpu-pool-claims-{pool_name}"))
            .map_err(|_| {
            Error::Configuration(format!(
                "GPU pool name {pool_name:?} holds a NUL byte and cannot name its claim table"
            ))
        })?;
        // SAFETY: a NUL-terminated name and documented flags.
        let raw_memfd = unsafe {
            libc::memfd_create(
                memfd_name.as_ptr(),
                libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING,
            )
        };
        if raw_memfd < 0 {
            return Err(claim_table_os_failure("memfd_create"));
        }
        // SAFETY: memfd_create returned a fresh fd nothing else owns.
        let memfd = unsafe { OwnedFd::from_raw_fd(raw_memfd) };
        // SAFETY: a valid fd and a non-negative length.
        if unsafe { libc::ftruncate(raw_memfd, GPU_POOL_CLAIM_TABLE_BYTES as libc::off_t) } != 0 {
            return Err(claim_table_os_failure("ftruncate"));
        }
        // SAFETY: a valid memfd created with MFD_ALLOW_SEALING.
        if unsafe {
            libc::fcntl(
                raw_memfd,
                libc::F_ADD_SEALS,
                libc::F_SEAL_SHRINK | libc::F_SEAL_GROW | libc::F_SEAL_SEAL,
            )
        } != 0
        {
            return Err(claim_table_os_failure("F_ADD_SEALS"));
        }
        Self::mapping(memfd)
    }

    /// Map a table another process created, from the fd it handed over.
    pub fn adopt(memfd: OwnedFd) -> Result<Self> {
        // SAFETY: an all-zero stat is a valid out-parameter.
        let mut memfd_status: libc::stat = unsafe { std::mem::zeroed() };
        // SAFETY: a valid fd and a valid out-parameter.
        if unsafe { libc::fstat(std::os::fd::AsRawFd::as_raw_fd(&memfd), &mut memfd_status) } != 0 {
            return Err(claim_table_os_failure("fstat"));
        }
        if memfd_status.st_size as usize != GPU_POOL_CLAIM_TABLE_BYTES {
            return Err(Error::Runtime(format!(
                "a GPU pool claim table fd is {} bytes; a claim table is {GPU_POOL_CLAIM_TABLE_BYTES}",
                memfd_status.st_size
            )));
        }
        Self::mapping(memfd)
    }

    fn mapping(memfd: OwnedFd) -> Result<Self> {
        // SAFETY: maps the whole of a memfd sized to one table, shared.
        let mapped = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                GPU_POOL_CLAIM_TABLE_BYTES,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                std::os::fd::AsRawFd::as_raw_fd(&memfd),
                0,
            )
        };
        if mapped == libc::MAP_FAILED {
            return Err(claim_table_os_failure("mmap"));
        }
        let rows = NonNull::new(mapped.cast::<GpuPoolClaimTableRow>())
            .ok_or_else(|| Error::Runtime("mmap of a GPU pool claim table answered null".into()))?;
        Ok(Self { memfd, rows })
    }

    /// Every row of the table.
    pub fn rows(&self) -> &[GpuPoolClaimTableRow] {
        // SAFETY: the mapping holds GPU_POOL_CLAIM_TABLE_ROWS page-aligned rows
        // of atomics, zeroed or written only through this type, for `self`'s life.
        unsafe { std::slice::from_raw_parts(self.rows.as_ptr(), GPU_POOL_CLAIM_TABLE_ROWS) }
    }

    /// The memfd, for handing the table to another process.
    pub fn memfd(&self) -> BorrowedFd<'_> {
        self.memfd.as_fd()
    }
}

impl Drop for GpuPoolClaimTableMemfd {
    fn drop(&mut self) {
        // SAFETY: unmaps exactly the mapping `mapping` made.
        unsafe { libc::munmap(self.rows.as_ptr().cast(), GPU_POOL_CLAIM_TABLE_BYTES) };
    }
}

fn claim_table_os_failure(call: &str) -> Error {
    Error::Runtime(format!(
        "GPU pool claim table {call}: {}",
        std::io::Error::last_os_error()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use streamlib_ipc_types::GpuPoolClaimantColumn;

    #[test]
    fn a_claim_taken_through_an_adopted_fd_holds_the_creators_row() {
        let created = GpuPoolClaimTableMemfd::create("adopted-claims").unwrap();
        let adopted =
            GpuPoolClaimTableMemfd::adopt(created.memfd().try_clone_to_owned().unwrap()).unwrap();
        created.rows()[5].publish_generation(9);

        adopted.rows()[5]
            .claim_frame(GpuPoolClaimantColumn::at(1).unwrap(), 9)
            .unwrap();

        assert!(created.rows()[5].is_claimed());
        assert!(!created.rows()[4].is_claimed());
    }

    #[test]
    fn an_fd_of_another_size_is_refused() {
        // SAFETY: a NUL-terminated name and documented flags.
        let raw = unsafe { libc::memfd_create(c"not-a-claim-table".as_ptr(), libc::MFD_CLOEXEC) };
        assert!(raw >= 0);
        // SAFETY: memfd_create returned a fresh fd.
        let wrong_size = unsafe { OwnedFd::from_raw_fd(raw) };
        // SAFETY: a valid fd.
        assert_eq!(unsafe { libc::ftruncate(raw, 4096) }, 0);

        let refusal = GpuPoolClaimTableMemfd::adopt(wrong_size).err().unwrap();

        assert!(refusal.to_string().contains("4096 bytes"), "{refusal}");
    }
}
