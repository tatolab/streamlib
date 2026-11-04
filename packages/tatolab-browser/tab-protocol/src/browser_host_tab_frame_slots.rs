// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The shared memory a tab's painted frames reach its node in: a fixed count
//! of slots, each one frame of packed BGRA at the tab's extent. The host
//! writes a slot only while the node does not hold it.

use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

/// Bytes one painted pixel takes in a slot.
pub const FRAME_SLOT_BYTES_PER_PIXEL: u64 = 4;

/// Bytes one slot takes for a frame of `width × height`.
pub fn frame_slot_byte_size(width: u32, height: u32) -> u64 {
    u64::from(width) * u64::from(height) * FRAME_SLOT_BYTES_PER_PIXEL
}

/// A tab's frame slots, mapped into this process.
pub struct BrowserHostTabFrameSlots {
    shared_memory: OwnedFd,
    mapped_base: *mut u8,
    mapped_byte_size: usize,
    slot_byte_size: usize,
    slot_count: u32,
}

// SAFETY: the mapping is plain shared memory; which side writes a slot when
// is the protocol's, never this type's, to keep.
unsafe impl Send for BrowserHostTabFrameSlots {}
unsafe impl Sync for BrowserHostTabFrameSlots {}

impl BrowserHostTabFrameSlots {
    /// Create `slot_count` slots of `width × height`, mapped for writing.
    pub fn create(slot_count: u32, width: u32, height: u32) -> std::io::Result<Self> {
        let slot_byte_size =
            usize::try_from(frame_slot_byte_size(width, height)).map_err(|_| {
                std::io::Error::other("a frame slot is past this machine's address space")
            })?;
        let mapped_byte_size =
            slot_byte_size
                .checked_mul(slot_count as usize)
                .ok_or_else(|| {
                    std::io::Error::other("the frame slots are past this machine's address space")
                })?;
        if mapped_byte_size == 0 {
            return Err(std::io::Error::other(
                "a tab needs at least one frame slot of 1x1",
            ));
        }
        let shared_memory = create_anonymous_shared_memory(mapped_byte_size)?;
        let mapped_base = map(
            &shared_memory,
            mapped_byte_size,
            libc::PROT_READ | libc::PROT_WRITE,
        )?;
        Ok(Self {
            shared_memory,
            mapped_base,
            mapped_byte_size,
            slot_byte_size,
            slot_count,
        })
    }

    /// Map the slots the host handed over as `shared_memory`, for reading.
    pub fn map_handed_over(
        shared_memory: OwnedFd,
        slot_count: u32,
        width: u32,
        height: u32,
    ) -> std::io::Result<Self> {
        let slot_byte_size =
            usize::try_from(frame_slot_byte_size(width, height)).map_err(|_| {
                std::io::Error::other("a frame slot is past this machine's address space")
            })?;
        let mapped_byte_size =
            slot_byte_size
                .checked_mul(slot_count as usize)
                .ok_or_else(|| {
                    std::io::Error::other("the frame slots are past this machine's address space")
                })?;
        // SAFETY: fstat on a descriptor this function owns.
        let mut status = std::mem::MaybeUninit::<libc::stat>::uninit();
        if unsafe { libc::fstat(shared_memory.as_raw_fd(), status.as_mut_ptr()) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: fstat succeeded, so it filled `status`.
        let handed_over_byte_size = unsafe { status.assume_init() }.st_size as u64;
        if handed_over_byte_size < mapped_byte_size as u64 {
            return Err(std::io::Error::other(format!(
                "the host handed {handed_over_byte_size} bytes of frame slots, short of the \
                 {mapped_byte_size} {slot_count} slots of {width}x{height} take"
            )));
        }
        let mapped_base = map(&shared_memory, mapped_byte_size, libc::PROT_READ)?;
        Ok(Self {
            shared_memory,
            mapped_base,
            mapped_byte_size,
            slot_byte_size,
            slot_count,
        })
    }

    /// The descriptor the node maps these slots from.
    pub fn shared_memory_descriptor(&self) -> std::os::fd::RawFd {
        self.shared_memory.as_raw_fd()
    }

    pub fn slot_count(&self) -> u32 {
        self.slot_count
    }

    /// Slot `slot`'s bytes, for the side that holds it.
    ///
    /// # Safety
    ///
    /// The other process does not write `slot` while the returned slice lives.
    pub unsafe fn slot(&self, slot: u32) -> Option<&[u8]> {
        (slot < self.slot_count).then(|| {
            // SAFETY: `slot` is inside the mapping, which lives as long as `self`.
            unsafe {
                std::slice::from_raw_parts(
                    self.mapped_base.add(slot as usize * self.slot_byte_size),
                    self.slot_byte_size,
                )
            }
        })
    }

    /// Slot `slot`'s bytes for writing, on the side that created the slots.
    ///
    /// # Safety
    ///
    /// The node does not hold `slot`, and nothing else in this process
    /// touches it while the returned slice lives.
    #[allow(clippy::mut_from_ref)]
    pub unsafe fn slot_for_writing(&self, slot: u32) -> Option<&mut [u8]> {
        (slot < self.slot_count).then(|| {
            // SAFETY: the caller's contract; the mapping is writable on the
            // creating side.
            unsafe {
                std::slice::from_raw_parts_mut(
                    self.mapped_base.add(slot as usize * self.slot_byte_size),
                    self.slot_byte_size,
                )
            }
        })
    }
}

impl Drop for BrowserHostTabFrameSlots {
    fn drop(&mut self) {
        // SAFETY: unmaps exactly the mapping this value made.
        unsafe {
            libc::munmap(self.mapped_base.cast(), self.mapped_byte_size);
        }
    }
}

fn map(
    shared_memory: &OwnedFd,
    byte_size: usize,
    protection: libc::c_int,
) -> std::io::Result<*mut u8> {
    // SAFETY: a fresh shared mapping of a descriptor at least `byte_size` long.
    let mapped = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            byte_size,
            protection,
            libc::MAP_SHARED,
            shared_memory.as_raw_fd(),
            0,
        )
    };
    if mapped == libc::MAP_FAILED {
        return Err(std::io::Error::last_os_error());
    }
    Ok(mapped.cast())
}

#[cfg(target_os = "linux")]
fn create_anonymous_shared_memory(byte_size: usize) -> std::io::Result<OwnedFd> {
    // SAFETY: memfd_create with a static NUL-terminated name.
    let descriptor =
        unsafe { libc::memfd_create(c"tatolab-browser-frame-slots".as_ptr(), libc::MFD_CLOEXEC) };
    if descriptor < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: memfd_create returned a descriptor this function alone owns.
    let shared_memory = unsafe { OwnedFd::from_raw_fd(descriptor) };
    size(&shared_memory, byte_size)?;
    Ok(shared_memory)
}

#[cfg(target_os = "macos")]
fn create_anonymous_shared_memory(byte_size: usize) -> std::io::Result<OwnedFd> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT_SHARED_MEMORY_NUMBER: AtomicU64 = AtomicU64::new(0);
    // macOS bounds a POSIX shared memory name at 31 bytes.
    let name = std::ffi::CString::new(format!(
        "/tbfs.{}.{}",
        std::process::id(),
        NEXT_SHARED_MEMORY_NUMBER.fetch_add(1, Ordering::Relaxed)
    ))
    .map_err(|_| std::io::Error::other("a shared memory name holds a NUL"))?;
    // SAFETY: shm_open with a NUL-terminated name; the name is unlinked at
    // once, so only descriptors keep the memory.
    let descriptor = unsafe {
        libc::shm_open(
            name.as_ptr(),
            libc::O_RDWR | libc::O_CREAT | libc::O_EXCL,
            0o600 as libc::c_uint,
        )
    };
    if descriptor < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: as above.
    unsafe {
        libc::shm_unlink(name.as_ptr());
        libc::fcntl(descriptor, libc::F_SETFD, libc::FD_CLOEXEC);
    }
    // SAFETY: shm_open returned a descriptor this function alone owns.
    let shared_memory = unsafe { OwnedFd::from_raw_fd(descriptor) };
    size(&shared_memory, byte_size)?;
    Ok(shared_memory)
}

fn size(shared_memory: &OwnedFd, byte_size: usize) -> std::io::Result<()> {
    // SAFETY: ftruncate on a descriptor the caller owns.
    if unsafe { libc::ftruncate(shared_memory.as_raw_fd(), byte_size as libc::off_t) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_the_host_writes_in_a_slot_the_node_reads() {
        let host_side = BrowserHostTabFrameSlots::create(3, 4, 2).unwrap();
        // SAFETY: no node holds any slot yet.
        unsafe { host_side.slot_for_writing(1).unwrap() }.fill(0x7F);
        let handed_over = duplicate_descriptor(host_side.shared_memory_descriptor());
        let node_side = BrowserHostTabFrameSlots::map_handed_over(handed_over, 3, 4, 2).unwrap();
        // SAFETY: the host wrote slot 1 before handing it over.
        let slot = unsafe { node_side.slot(1).unwrap() };
        assert_eq!(slot.len(), 4 * 2 * 4);
        assert!(slot.iter().all(|byte| *byte == 0x7F));
        // SAFETY: as above.
        assert!(
            unsafe { node_side.slot(0).unwrap() }
                .iter()
                .all(|byte| *byte == 0)
        );
        assert!(unsafe { node_side.slot(3) }.is_none());
    }

    #[test]
    fn slots_short_of_the_extent_are_refused() {
        let host_side = BrowserHostTabFrameSlots::create(1, 4, 2).unwrap();
        let handed_over = duplicate_descriptor(host_side.shared_memory_descriptor());
        let refusal = BrowserHostTabFrameSlots::map_handed_over(handed_over, 2, 4, 2)
            .err()
            .unwrap();
        assert!(refusal.to_string().contains("short of"), "{refusal}");
    }

    #[test]
    fn an_empty_tab_is_refused() {
        assert!(BrowserHostTabFrameSlots::create(0, 1920, 1080).is_err());
    }

    fn duplicate_descriptor(descriptor: std::os::fd::RawFd) -> OwnedFd {
        // SAFETY: dup of a live descriptor.
        let duplicated = unsafe { libc::dup(descriptor) };
        assert!(duplicated >= 0);
        // SAFETY: dup returned a descriptor this test owns.
        unsafe { OwnedFd::from_raw_fd(duplicated) }
    }
}
