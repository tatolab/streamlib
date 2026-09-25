// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The wheel's one way to open a kqueue.

use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

/// A new kqueue, close-on-exec before anything can use it.
pub(crate) fn open_a_close_on_exec_kqueue() -> std::io::Result<OwnedFd> {
    // SAFETY: kqueue returns -1 on failure; checked below.
    let raw_kqueue_fd = unsafe { libc::kqueue() };
    if raw_kqueue_fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: raw_kqueue_fd was just opened here and nothing else owns it.
    let kqueue_fd = unsafe { OwnedFd::from_raw_fd(raw_kqueue_fd) };
    // Darwin has no `kqueue1`, so close-on-exec is set before the fd is used.
    // SAFETY: a scalar syscall on an fd this function owns.
    if unsafe { libc::fcntl(kqueue_fd.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) } < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(kqueue_fd)
}
