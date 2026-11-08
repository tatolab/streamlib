// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! A GPU pool claim table's rows in a Mach memory entry, mapped shared so
//! every interpreter handed a send right to the entry claims in the same
//! memory.

use std::ptr::NonNull;

use mach2::kern_return::KERN_SUCCESS;
use mach2::port::{MACH_PORT_NULL, mach_port_t};
use mach2::traps::mach_task_self;
use mach2::vm::{mach_make_memory_entry_64, mach_vm_allocate, mach_vm_deallocate, mach_vm_map};
use mach2::vm_inherit::VM_INHERIT_NONE;
use mach2::vm_prot::{VM_PROT_READ, VM_PROT_WRITE};
use mach2::vm_statistics::VM_FLAGS_ANYWHERE;
use mach2::vm_types::{mach_vm_address_t, mach_vm_size_t};
use streamlib_surface_client::OwnedMachSendRight;

use crate::{GPU_POOL_CLAIM_TABLE_BYTES, GPU_POOL_CLAIM_TABLE_ROWS, GpuPoolClaimTableRow};

/// `MAP_MEM_VM_SHARE` from `<mach/memory_object_types.h>`: the entry shares
/// the pages of the mapping it names rather than copying them.
const MAP_MEM_VM_SHARE: i32 = 0x0040_0000;

/// A claim table's rows, in a Mach memory entry at least one table long —
/// whole pages, 16 KiB on Apple silicon — so no holder maps fewer bytes than
/// another writes.
pub struct GpuPoolClaimTableMachMemoryEntry {
    memory_entry: OwnedMachSendRight,
    rows: NonNull<GpuPoolClaimTableRow>,
}

// SAFETY: the mapping is shared memory of atomics only, read and written
// through `&GpuPoolClaimTableRow`, and unmapped once in `Drop`.
unsafe impl Send for GpuPoolClaimTableMachMemoryEntry {}
// SAFETY: as above; every access is an atomic operation.
unsafe impl Sync for GpuPoolClaimTableMachMemoryEntry {}

impl GpuPoolClaimTableMachMemoryEntry {
    /// A fresh table: zeroed rows, which publish nothing and hold no claims.
    pub fn create(_pool_name: &str) -> std::io::Result<Self> {
        let mut mapped_address: mach_vm_address_t = 0;
        // SAFETY: allocates fresh zero-filled pages anywhere in this task.
        let allocated = unsafe {
            mach_vm_allocate(
                mach_task_self(),
                &mut mapped_address,
                GPU_POOL_CLAIM_TABLE_BYTES as mach_vm_size_t,
                VM_FLAGS_ANYWHERE,
            )
        };
        if allocated != KERN_SUCCESS {
            return Err(claim_table_mach_failure("mach_vm_allocate", allocated));
        }
        let mut entry_size = GPU_POOL_CLAIM_TABLE_BYTES as u64;
        let mut memory_entry: mach_port_t = MACH_PORT_NULL;
        // SAFETY: names the pages just allocated; the out-parameters are valid.
        let made = unsafe {
            mach_make_memory_entry_64(
                mach_task_self(),
                &mut entry_size,
                mapped_address,
                VM_PROT_READ | VM_PROT_WRITE | MAP_MEM_VM_SHARE,
                &mut memory_entry,
                MACH_PORT_NULL,
            )
        };
        if made != KERN_SUCCESS {
            // SAFETY: frees exactly the allocation above.
            unsafe {
                mach_vm_deallocate(
                    mach_task_self(),
                    mapped_address,
                    GPU_POOL_CLAIM_TABLE_BYTES as mach_vm_size_t,
                )
            };
            return Err(claim_table_mach_failure("mach_make_memory_entry_64", made));
        }
        // SAFETY: mach_make_memory_entry_64 handed this task one send right.
        let memory_entry = unsafe { OwnedMachSendRight::from_raw_name(memory_entry) };
        if (entry_size as usize) < GPU_POOL_CLAIM_TABLE_BYTES {
            // SAFETY: frees exactly the allocation above.
            unsafe {
                mach_vm_deallocate(
                    mach_task_self(),
                    mapped_address,
                    GPU_POOL_CLAIM_TABLE_BYTES as mach_vm_size_t,
                )
            };
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "a GPU pool claim table's memory entry is {entry_size} bytes, short of the \
                     {GPU_POOL_CLAIM_TABLE_BYTES} a claim table spans"
                ),
            ));
        }
        Self::over(memory_entry, mapped_address)
    }

    /// Map a table another process created, from the memory entry it handed
    /// over.
    pub fn adopt(memory_entry: OwnedMachSendRight) -> std::io::Result<Self> {
        let mut mapped_address: mach_vm_address_t = 0;
        // SAFETY: maps one table's bytes of the entry, shared and read-write,
        // anywhere in this task; a smaller entry is refused by the kernel.
        let mapped = unsafe {
            mach_vm_map(
                mach_task_self(),
                &mut mapped_address,
                GPU_POOL_CLAIM_TABLE_BYTES as mach_vm_size_t,
                0,
                VM_FLAGS_ANYWHERE,
                memory_entry.as_raw_name(),
                0,
                0,
                VM_PROT_READ | VM_PROT_WRITE,
                VM_PROT_READ | VM_PROT_WRITE,
                VM_INHERIT_NONE,
            )
        };
        if mapped != KERN_SUCCESS {
            return Err(claim_table_mach_failure("mach_vm_map", mapped));
        }
        Self::over(memory_entry, mapped_address)
    }

    fn over(
        memory_entry: OwnedMachSendRight,
        mapped_address: mach_vm_address_t,
    ) -> std::io::Result<Self> {
        let rows = NonNull::new(mapped_address as usize as *mut GpuPoolClaimTableRow)
            .ok_or_else(|| std::io::Error::other("a GPU pool claim table mapped at null"))?;
        Ok(Self { memory_entry, rows })
    }

    /// Every row of the table.
    pub fn rows(&self) -> &[GpuPoolClaimTableRow] {
        // SAFETY: the mapping holds GPU_POOL_CLAIM_TABLE_ROWS page-aligned rows
        // of atomics, zeroed or written only through this type, for `self`'s life.
        unsafe { std::slice::from_raw_parts(self.rows.as_ptr(), GPU_POOL_CLAIM_TABLE_ROWS) }
    }

    /// A fresh send right to the memory entry, for handing the table to
    /// another process.
    pub fn memory_entry_send_right(&self) -> std::io::Result<OwnedMachSendRight> {
        self.memory_entry.try_clone()
    }
}

impl Drop for GpuPoolClaimTableMachMemoryEntry {
    fn drop(&mut self) {
        // SAFETY: unmaps exactly the mapping `create` or `adopt` made.
        unsafe {
            mach_vm_deallocate(
                mach_task_self(),
                self.rows.as_ptr() as usize as mach_vm_address_t,
                GPU_POOL_CLAIM_TABLE_BYTES as mach_vm_size_t,
            )
        };
    }
}

fn claim_table_mach_failure(call: &str, kern_return: i32) -> std::io::Error {
    std::io::Error::other(format!(
        "GPU pool claim table {call} answered kern_return {kern_return:#x}"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::GpuPoolClaimantColumn;

    #[test]
    fn a_claim_taken_through_an_adopted_memory_entry_holds_the_creators_row() {
        let created = GpuPoolClaimTableMachMemoryEntry::create("adopted-claims").unwrap();
        let adopted =
            GpuPoolClaimTableMachMemoryEntry::adopt(created.memory_entry_send_right().unwrap())
                .unwrap();
        created.rows()[5].publish_generation(9);

        adopted.rows()[5]
            .claim_frame(GpuPoolClaimantColumn::at(1).unwrap(), 9)
            .unwrap();

        assert!(created.rows()[5].is_claimed());
        assert!(!created.rows()[4].is_claimed());
    }

    #[test]
    fn a_mapping_outlives_the_table_that_handed_it_over() {
        let created = GpuPoolClaimTableMachMemoryEntry::create("outlived-claims").unwrap();
        let adopted =
            GpuPoolClaimTableMachMemoryEntry::adopt(created.memory_entry_send_right().unwrap())
                .unwrap();
        created.rows()[2].publish_generation(3);
        drop(created);

        adopted.rows()[2]
            .claim_frame(GpuPoolClaimantColumn::at(0).unwrap(), 3)
            .unwrap();

        assert!(adopted.rows()[2].is_claimed());
    }
}
