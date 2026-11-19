// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A descriptor one thread makes readable to end another's wait in an epoll set or a kqueue:
//! an eventfd on Linux, a close-on-exec pipe on macOS, which has no eventfd. It stays readable
//! until drained, so a wake that lands before the wait begins is never missed.

use std::os::fd::{BorrowedFd, OwnedFd};

/// An eventfd on Linux: one descriptor both written and waited on.
#[cfg(target_os = "linux")]
pub(crate) struct ReadableWakeDescriptor {
    eventfd: OwnedFd,
}

#[cfg(target_os = "linux")]
impl ReadableWakeDescriptor {
    pub(crate) fn create() -> std::io::Result<Self> {
        use std::os::fd::FromRawFd;
        // SAFETY: eventfd returns -1 on failure; checked below. Its counter starts at 0;
        // EFD_CLOEXEC keeps it out of child processes, and EFD_NONBLOCK lets a drain of a
        // counter at 0 return rather than block.
        let raw = unsafe { libc::eventfd(0, libc::EFD_CLOEXEC | libc::EFD_NONBLOCK) };
        if raw < 0 {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: raw is a fresh, owned fd from a successful eventfd() call.
        Ok(Self {
            eventfd: unsafe { OwnedFd::from_raw_fd(raw) },
        })
    }

    pub(crate) fn make_readable(&self) -> std::io::Result<()> {
        use std::os::fd::AsRawFd;
        let buf = 1u64.to_ne_bytes();
        // SAFETY: the eventfd is owned by Self for the duration of this call, and takes an
        // 8-byte write.
        let n = unsafe { libc::write(self.eventfd.as_raw_fd(), buf.as_ptr().cast(), buf.len()) };
        if n < 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }

    /// Leave the descriptor not readable until the next [`Self::make_readable`].
    pub(crate) fn drain(&self) {
        use std::os::fd::AsRawFd;
        let mut buf = [0u8; 8];
        // SAFETY: the eventfd is owned by Self for the duration of this call; a read of 8
        // bytes takes the whole counter, and on a counter at 0 fails with EAGAIN, which leaves
        // nothing to drain.
        let _ = unsafe { libc::read(self.eventfd.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len()) };
    }

    /// The descriptor to wait on, borrowed for as long as `self` lives.
    pub(crate) fn readable_end(&self) -> BorrowedFd<'_> {
        use std::os::fd::AsFd;
        self.eventfd.as_fd()
    }

    pub(crate) fn try_clone_readable_end(&self) -> std::io::Result<OwnedFd> {
        self.eventfd.try_clone()
    }
}

/// A close-on-exec, non-blocking pipe on macOS. At most one byte is written while the pipe
/// holds one not yet drained, so a repeated wake never fills it.
#[cfg(target_os = "macos")]
pub(crate) struct ReadableWakeDescriptor {
    pipe_read_end: std::io::PipeReader,
    pipe_write_end: std::io::PipeWriter,
    a_byte_waits: std::sync::atomic::AtomicBool,
}

#[cfg(target_os = "macos")]
impl ReadableWakeDescriptor {
    pub(crate) fn create() -> std::io::Result<Self> {
        use std::os::fd::AsRawFd;
        let (pipe_read_end, pipe_write_end) = std::io::pipe()?;
        for end in [pipe_read_end.as_raw_fd(), pipe_write_end.as_raw_fd()] {
            // SAFETY: fcntl on a live fd this function owns.
            let flags = unsafe { libc::fcntl(end, libc::F_GETFL) };
            // SAFETY: as above.
            if flags < 0 || unsafe { libc::fcntl(end, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0
            {
                return Err(std::io::Error::last_os_error());
            }
        }
        Ok(Self {
            pipe_read_end,
            pipe_write_end,
            a_byte_waits: std::sync::atomic::AtomicBool::new(false),
        })
    }

    pub(crate) fn make_readable(&self) -> std::io::Result<()> {
        use std::io::Write;
        if self
            .a_byte_waits
            .swap(true, std::sync::atomic::Ordering::AcqRel)
        {
            return Ok(());
        }
        (&self.pipe_write_end).write_all(&[1]).inspect_err(|_| {
            self.a_byte_waits
                .store(false, std::sync::atomic::Ordering::Release)
        })
    }

    /// Leave the descriptor not readable until the next [`Self::make_readable`]. The flag is
    /// cleared before the pipe is read, so a wake landing during the drain writes its own byte.
    pub(crate) fn drain(&self) {
        use std::io::Read;
        self.a_byte_waits
            .store(false, std::sync::atomic::Ordering::Release);
        let mut buf = [0u8; 16];
        while matches!((&self.pipe_read_end).read(&mut buf), Ok(read) if read > 0) {}
    }

    /// The descriptor to wait on, borrowed for as long as `self` lives.
    pub(crate) fn readable_end(&self) -> BorrowedFd<'_> {
        use std::os::fd::AsFd;
        self.pipe_read_end.as_fd()
    }

    pub(crate) fn try_clone_readable_end(&self) -> std::io::Result<OwnedFd> {
        self.pipe_read_end.try_clone().map(OwnedFd::from)
    }
}

#[cfg(test)]
mod tests;
