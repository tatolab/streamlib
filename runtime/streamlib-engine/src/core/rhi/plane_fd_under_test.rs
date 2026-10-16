// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! A pipe standing in for a plane fd a test hands to code that must close it.

/// A pipe's read end, handed over as a plane fd. Each pipe has its own inode,
/// and holding the write end keeps that inode alive after the read end
/// closes, so "was it closed?" is answered without racing a parallel test
/// thread for the recycled number.
pub(crate) struct PlaneFdUnderTest {
    pub(crate) plane_fd: std::os::unix::io::RawFd,
    write_end_fd: std::os::unix::io::RawFd,
    inode: u64,
}

fn inode_of(fd: std::os::unix::io::RawFd) -> Option<u64> {
    let mut file_status = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: fstat writes into the buffer it is given and touches nothing else.
    if unsafe { libc::fstat(fd, file_status.as_mut_ptr()) } != 0 {
        return None;
    }
    // SAFETY: fstat returned 0, so it filled the buffer.
    Some(unsafe { file_status.assume_init() }.st_ino as u64)
}

impl PlaneFdUnderTest {
    /// A fresh pipe whose read end is the plane fd.
    pub(crate) fn mint() -> Self {
        let mut pipe_ends = [0 as std::os::unix::io::RawFd; 2];
        // SAFETY: pipe2 writes two fds into a two-element array.
        assert_eq!(
            unsafe { libc::pipe2(pipe_ends.as_mut_ptr(), libc::O_CLOEXEC) },
            0
        );
        let inode = inode_of(pipe_ends[0]).expect("a fresh pipe must stat");
        Self {
            plane_fd: pipe_ends[0],
            write_end_fd: pipe_ends[1],
            inode,
        }
    }

    /// The plane fd's number no longer names this pipe: closed outright, or
    /// recycled onto another file.
    pub(crate) fn was_closed(&self) -> bool {
        inode_of(self.plane_fd) != Some(self.inode)
    }
}

impl Drop for PlaneFdUnderTest {
    fn drop(&mut self) {
        // SAFETY: the write end is ours; the read end is closed only if it
        // still names this pipe, so a recycled number is never touched.
        unsafe {
            if !self.was_closed() {
                libc::close(self.plane_fd);
            }
            libc::close(self.write_end_fd);
        }
    }
}
