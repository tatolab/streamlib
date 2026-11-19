// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use crossbeam_channel::{Receiver, Sender};

#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::os::fd::OwnedFd;

use streamlib_runtime_client_contract::local_api_wire_contract::NodeShutdownChannelOnTheWire;

use super::GraphRenders;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use crate::core::execution::readable_wake_descriptor::ReadableWakeDescriptor;
use crate::core::json_schema::GraphNodeComponentsOnTheWire;

pub struct ShutdownChannelComponent {
    pub sender: Sender<()>,
    pub receiver: Option<Receiver<()>>,
    /// Descriptor that turns readable on shutdown and, never drained, stays so. The reactive
    /// thread runner waits on it beside its listener; continuous and manual
    /// modes use the crossbeam channel via [`Self::sender`] / [`Self::receiver`].
    /// `None` when it could not be created, which leaves a reactive runner on
    /// channel-only shutdown.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    shutdown_wake: Option<ReadableWakeDescriptor>,
}

impl ShutdownChannelComponent {
    pub fn new() -> Self {
        let (sender, receiver) = crossbeam_channel::bounded(1);
        Self {
            sender,
            receiver: Some(receiver),
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            shutdown_wake: ReadableWakeDescriptor::create()
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

impl Default for ShutdownChannelComponent {
    fn default() -> Self {
        Self::new()
    }
}

impl GraphRenders<GraphNodeComponentsOnTheWire> for ShutdownChannelComponent {
    fn render_onto(&self, components: &mut GraphNodeComponentsOnTheWire) {
        components.shutdown_channel = Some(NodeShutdownChannelOnTheWire {
            receiver_taken: self.receiver.is_none(),
        });
    }
}
