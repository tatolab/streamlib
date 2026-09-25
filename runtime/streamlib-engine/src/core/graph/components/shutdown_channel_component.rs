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
    /// `None` when it could not be created, which leaves a reactive runner on
    /// channel-only shutdown.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    shutdown_wake: Option<ShutdownWakeDescriptor>,
}

impl ShutdownChannelComponent {
    pub fn new() -> Self {
        let (sender, receiver) = crossbeam_channel::bounded(1);
        Self {
            sender,
            receiver: Some(receiver),
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            shutdown_wake: ShutdownWakeDescriptor::create()
                .inspect_err(|e| {
                    tracing::warn!(
                        "shutdown wake descriptor creation failed, reactive runners fall \
                         back to channel-only shutdown: {}",
                        e
                    )
                })
                .ok(),
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
        if let Some(shutdown_wake) = &self.shutdown_wake
            && let Err(e) = shutdown_wake.make_readable()
        {
            tracing::warn!("shutdown wake descriptor write failed: {}", e);
        }
        let _ = self.sender.send(());
    }

    /// Duplicate the readable end of the shutdown wake descriptor for a
    /// consumer that registers it in its own epoll set or kqueue.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn try_clone_shutdown_wake_fd(&self) -> std::io::Result<OwnedFd> {
        match &self.shutdown_wake {
            Some(shutdown_wake) => shutdown_wake.try_clone_readable_end(),
            None => Err(std::io::Error::other(
                "the shutdown wake descriptor was never created",
            )),
        }
    }
}

/// An eventfd on Linux: one descriptor both written and waited on.
#[cfg(target_os = "linux")]
struct ShutdownWakeDescriptor {
    eventfd: OwnedFd,
}

#[cfg(target_os = "linux")]
impl ShutdownWakeDescriptor {
    fn create() -> std::io::Result<Self> {
        use std::os::fd::FromRawFd;
        // SAFETY: eventfd returns -1 on failure; checked below. Initial counter
        // is 0; EFD_CLOEXEC prevents fork-inherited duplicates from leaking
        // into subprocesses.
        let raw = unsafe { libc::eventfd(0, libc::EFD_CLOEXEC) };
        if raw < 0 {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: raw is a fresh, owned fd from a successful eventfd() call.
        Ok(Self {
            eventfd: unsafe { OwnedFd::from_raw_fd(raw) },
        })
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
/// the one byte written leaves the read end readable for good.
#[cfg(target_os = "macos")]
struct ShutdownWakeDescriptor {
    pipe_read_end: std::io::PipeReader,
    pipe_write_end: std::io::PipeWriter,
    already_signalled: std::sync::atomic::AtomicBool,
}

#[cfg(target_os = "macos")]
impl ShutdownWakeDescriptor {
    fn create() -> std::io::Result<Self> {
        let (pipe_read_end, pipe_write_end) = std::io::pipe()?;
        Ok(Self {
            pipe_read_end,
            pipe_write_end,
            already_signalled: std::sync::atomic::AtomicBool::new(false),
        })
    }

    /// Writes once however often it is called, so a repeated signal never
    /// fills the pipe and blocks.
    fn make_readable(&self) -> std::io::Result<()> {
        use std::io::Write;
        if self
            .already_signalled
            .swap(true, std::sync::atomic::Ordering::AcqRel)
        {
            return Ok(());
        }
        (&self.pipe_write_end).write_all(&[1]).inspect_err(|_| {
            self.already_signalled
                .store(false, std::sync::atomic::Ordering::Release)
        })
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
