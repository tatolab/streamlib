// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use crossbeam_channel::{Receiver, Sender};
use serde_json::Value as JsonValue;

#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::os::fd::OwnedFd;

use super::JsonSerializableComponent;

/// Channel to signal processor shutdown.
pub struct ShutdownChannelComponent {
    pub sender: Sender<()>,
    pub receiver: Option<Receiver<()>>,
    /// Descriptor that turns readable on shutdown and stays so. The reactive
    /// thread runner waits on it beside its listener; continuous and manual
    /// modes use the crossbeam channel via [`Self::sender`] / [`Self::receiver`].
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    shutdown_wake: ShutdownWakeDescriptor,
}

impl ShutdownChannelComponent {
    pub fn new() -> Self {
        let (sender, receiver) = crossbeam_channel::bounded(1);
        Self {
            sender,
            receiver: Some(receiver),
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            shutdown_wake: ShutdownWakeDescriptor::create(),
        }
    }

    /// Take the receiver (can only be done once).
    pub fn take_receiver(&mut self) -> Option<Receiver<()>> {
        self.receiver.take()
    }

    /// Signal shutdown to every waiting consumer: makes the shutdown wake
    /// descriptor readable (ends a reactive wait) and sends on the crossbeam
    /// channel (poll-based continuous/manual modes).
    pub fn signal_shutdown(&self) {
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        if let Err(e) = self.shutdown_wake.make_readable() {
            tracing::warn!("shutdown wake descriptor write failed: {}", e);
        }
        let _ = self.sender.send(());
    }

    /// Duplicate the readable end of the shutdown wake descriptor for a
    /// consumer that registers it in its own epoll set or kqueue.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn try_clone_shutdown_wake_fd(&self) -> std::io::Result<OwnedFd> {
        self.shutdown_wake.try_clone_readable_end()
    }
}

/// An eventfd on Linux: one descriptor both written and waited on.
#[cfg(target_os = "linux")]
struct ShutdownWakeDescriptor {
    eventfd: OwnedFd,
}

#[cfg(target_os = "linux")]
impl ShutdownWakeDescriptor {
    fn create() -> Self {
        use std::os::fd::FromRawFd;
        // SAFETY: eventfd returns -1 on failure; checked below. Initial counter
        // is 0; EFD_CLOEXEC prevents fork-inherited duplicates from leaking
        // into subprocesses.
        let raw = unsafe { libc::eventfd(0, libc::EFD_CLOEXEC) };
        if raw < 0 {
            // The runtime can't shut reactive processors down without it.
            panic!(
                "eventfd(EFD_CLOEXEC) failed: {}",
                std::io::Error::last_os_error()
            );
        }
        // SAFETY: raw is a fresh, owned fd from a successful eventfd() call.
        Self {
            eventfd: unsafe { OwnedFd::from_raw_fd(raw) },
        }
    }

    fn make_readable(&self) -> std::io::Result<()> {
        use std::os::fd::AsRawFd;
        let buf = 1u64.to_ne_bytes();
        // SAFETY: the eventfd is owned by Self for the duration of this call.
        // eventfd accepts an 8-byte write.
        let n = unsafe { libc::write(self.eventfd.as_raw_fd(), buf.as_ptr().cast(), buf.len()) };
        if n < 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }

    fn try_clone_readable_end(&self) -> std::io::Result<OwnedFd> {
        self.eventfd.try_clone()
    }
}

/// A close-on-exec pipe on macOS, which has no eventfd. Nothing reads it, so
/// the first byte written leaves the read end readable for good.
#[cfg(target_os = "macos")]
struct ShutdownWakeDescriptor {
    pipe_read_end: std::io::PipeReader,
    pipe_write_end: std::io::PipeWriter,
}

#[cfg(target_os = "macos")]
impl ShutdownWakeDescriptor {
    fn create() -> Self {
        use std::os::fd::AsRawFd;
        let (pipe_read_end, pipe_write_end) = std::io::pipe().unwrap_or_else(|e| {
            // The runtime can't shut reactive processors down without it.
            panic!("pipe() for the shutdown wake failed: {e}")
        });
        // A repeated signal against a full pipe returns EAGAIN rather than
        // blocking the thread tearing the graph down.
        // SAFETY: fcntl on a live fd this function owns.
        let flags = unsafe { libc::fcntl(pipe_write_end.as_raw_fd(), libc::F_GETFL) };
        // SAFETY: as above.
        if flags < 0
            || unsafe {
                libc::fcntl(
                    pipe_write_end.as_raw_fd(),
                    libc::F_SETFL,
                    flags | libc::O_NONBLOCK,
                )
            } < 0
        {
            panic!(
                "O_NONBLOCK on the shutdown wake pipe failed: {}",
                std::io::Error::last_os_error()
            );
        }
        Self {
            pipe_read_end,
            pipe_write_end,
        }
    }

    fn make_readable(&self) -> std::io::Result<()> {
        use std::io::Write;
        match (&self.pipe_write_end).write(&[1]) {
            Ok(_) => Ok(()),
            // Full means already readable.
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => Ok(()),
            Err(e) => Err(e),
        }
    }

    fn try_clone_readable_end(&self) -> std::io::Result<OwnedFd> {
        self.pipe_read_end.try_clone().map(OwnedFd::from)
    }
}

impl Default for ShutdownChannelComponent {
    fn default() -> Self {
        Self::new()
    }
}

impl JsonSerializableComponent for ShutdownChannelComponent {
    fn json_key(&self) -> &'static str {
        "shutdown_channel"
    }

    fn to_json(&self) -> JsonValue {
        serde_json::json!({
            "attached": true,
            "receiver_taken": self.receiver.is_none()
        })
    }
}
