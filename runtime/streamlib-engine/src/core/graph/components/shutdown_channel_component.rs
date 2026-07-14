// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use crossbeam_channel::{Receiver, Sender};
use serde_json::Value as JsonValue;

#[cfg(target_os = "linux")]
use std::os::fd::{AsRawFd, OwnedFd};

use super::JsonSerializableComponent;

/// Channel to signal processor shutdown.
pub struct ShutdownChannelComponent {
    pub sender: Sender<()>,
    pub receiver: Option<Receiver<()>>,
    /// Linux-only: eventfd that mirrors the shutdown signal. The reactive
    /// thread runner registers this fd in its epoll set so it can wake on
    /// shutdown without polling. Continuous and manual modes still use the
    /// crossbeam channel via [`Self::sender`] / [`Self::receiver`].
    #[cfg(target_os = "linux")]
    shutdown_eventfd: OwnedFd,
}

impl ShutdownChannelComponent {
    pub fn new() -> Self {
        let (sender, receiver) = crossbeam_channel::bounded(1);
        Self {
            sender,
            receiver: Some(receiver),
            #[cfg(target_os = "linux")]
            shutdown_eventfd: create_shutdown_eventfd(),
        }
    }

    /// Take the receiver (can only be done once).
    pub fn take_receiver(&mut self) -> Option<Receiver<()>> {
        self.receiver.take()
    }

    /// Signal shutdown to every waiting consumer: writes to the Linux
    /// eventfd (wakes the reactive epoll-wait) and sends on the crossbeam
    /// channel (poll-based continuous/manual modes).
    pub fn signal_shutdown(&self) {
        #[cfg(target_os = "linux")]
        {
            let buf = 1u64.to_ne_bytes();
            // SAFETY: shutdown_eventfd is a valid eventfd owned by Self for
            // the duration of this call. eventfd accepts an 8-byte write.
            let n = unsafe {
                libc::write(
                    self.shutdown_eventfd.as_raw_fd(),
                    buf.as_ptr().cast(),
                    buf.len(),
                )
            };
            if n < 0 {
                tracing::warn!(
                    "shutdown eventfd write failed: {}",
                    std::io::Error::last_os_error()
                );
            }
        }
        let _ = self.sender.send(());
    }

    /// Linux-only: duplicate the shutdown eventfd for a consumer that
    /// wants to register it in its own epoll set. Returns an [`OwnedFd`]
    /// the caller closes when done.
    #[cfg(target_os = "linux")]
    pub fn try_clone_shutdown_eventfd(&self) -> std::io::Result<OwnedFd> {
        self.shutdown_eventfd.try_clone()
    }
}

#[cfg(target_os = "linux")]
fn create_shutdown_eventfd() -> OwnedFd {
    use std::os::fd::FromRawFd;
    // SAFETY: eventfd returns -1 on failure; checked below. Initial counter
    // is 0; EFD_CLOEXEC prevents fork-inherited duplicates from leaking
    // into subprocesses.
    let raw = unsafe { libc::eventfd(0, libc::EFD_CLOEXEC) };
    if raw < 0 {
        // eventfd failure here is unrecoverable — the runtime can't shutdown
        // reactive processors without it. Panicking surfaces the misconfig
        // immediately instead of silently degrading to the old polling shape.
        panic!(
            "eventfd(EFD_CLOEXEC) failed: {}",
            std::io::Error::last_os_error()
        );
    }
    // SAFETY: raw is a fresh, owned fd from a successful eventfd() call.
    unsafe { OwnedFd::from_raw_fd(raw) }
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
