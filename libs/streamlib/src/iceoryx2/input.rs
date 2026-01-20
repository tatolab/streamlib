// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Input mailboxes for receiving frames from upstream processors.

use std::cell::UnsafeCell;
use std::collections::HashMap;

use iceoryx2::port::subscriber::Subscriber;
use iceoryx2::prelude::*;
use serde::de::DeserializeOwned;

use super::mailbox::PortMailbox;
use super::read_mode::ReadMode;
use super::FramePayload;
use crate::core::error::{Result, StreamError};

/// Thread-local subscriber wrapper.
///
/// # Safety
/// This wrapper is safe to send between threads because:
/// 1. The Subscriber is only ever set AFTER the processor is spawned on its execution thread
/// 2. Once set, the Subscriber is only accessed from that same thread
/// 3. The wrapper starts with `None` and is populated during wiring on the target thread
struct SendableSubscriber(UnsafeCell<Option<Subscriber<ipc::Service, FramePayload, ()>>>);

// SAFETY: The Subscriber is only accessed from a single thread after being set.
// The processor lifecycle ensures that:
// 1. InputMailboxes is created with subscriber = None (safe to send)
// 2. After spawn, the processor is on its execution thread
// 3. set_subscriber() is called from that execution thread during wiring
// 4. All subsequent access is from the same thread
unsafe impl Send for SendableSubscriber {}

impl SendableSubscriber {
    fn new() -> Self {
        Self(UnsafeCell::new(None))
    }

    fn set(&self, subscriber: Subscriber<ipc::Service, FramePayload, ()>) {
        // SAFETY: Only called from the processor's execution thread after spawn
        unsafe {
            *self.0.get() = Some(subscriber);
        }
    }

    fn get(&self) -> Option<&Subscriber<ipc::Service, FramePayload, ()>> {
        // SAFETY: Only called from the processor's execution thread
        unsafe { (*self.0.get()).as_ref() }
    }
}

/// Per-port configuration: mailbox and read mode.
struct PortConfig {
    mailbox: PortMailbox,
    read_mode: ReadMode,
}

/// Collection of input mailboxes, one per input port.
///
/// The mailsorter task routes incoming payloads to the appropriate mailbox
/// based on the port_key in the payload.
pub struct InputMailboxes {
    ports: HashMap<String, PortConfig>,
    subscriber: SendableSubscriber,
}

impl InputMailboxes {
    /// Create a new empty collection of input mailboxes.
    pub fn new() -> Self {
        Self {
            ports: HashMap::new(),
            subscriber: SendableSubscriber::new(),
        }
    }

    /// Add a mailbox for the given port with the specified buffer size and read mode.
    pub fn add_port(&mut self, port: &str, buffer_size: usize, read_mode: ReadMode) {
        self.ports.insert(
            port.to_string(),
            PortConfig {
                mailbox: PortMailbox::new(buffer_size),
                read_mode,
            },
        );
    }

    /// Set the iceoryx2 Subscriber for receiving payloads.
    ///
    /// Note: This should only be called from the processor's execution thread.
    pub fn set_subscriber(&self, subscriber: Subscriber<ipc::Service, FramePayload, ()>) {
        self.subscriber.set(subscriber);
    }

    /// Receive all pending payloads from the iceoryx2 Subscriber and route them to mailboxes.
    ///
    /// This is called automatically by `pop()` and `has_data()`, but can be called
    /// explicitly if needed.
    pub fn receive_pending(&mut self) {
        // Collect payloads first to avoid borrow conflicts
        let payloads: Vec<FramePayload> = {
            let Some(subscriber) = self.subscriber.get() else {
                return;
            };

            let mut collected = Vec::new();
            while let Ok(Some(sample)) = subscriber.receive() {
                collected.push(*sample.payload());
            }
            collected
        };

        // Route all collected payloads to mailboxes
        for payload in payloads {
            self.route(payload);
        }
    }

    /// Get the most recent payload for the given port.
    ///
    /// Returns None if no payload is available or the port doesn't exist.
    /// Note: This does NOT receive pending data first.
    pub fn peek(&self, port: &str) -> Option<&FramePayload> {
        self.ports.get(port).and_then(|p| p.mailbox.peek())
    }

    /// Read and deserialize a frame from the given port.
    ///
    /// Uses the port's read mode to determine consumption strategy:
    /// - `SkipToLatest`: Drains buffer, returns only the newest frame (video)
    /// - `ReadNextInOrder`: Returns oldest frame in FIFO order (audio)
    ///
    /// This first receives any pending data from the iceoryx2 Subscriber,
    /// routes it to the appropriate mailboxes, then reads from the requested port.
    pub fn read<T: DeserializeOwned>(&mut self, port: &str) -> Result<T> {
        self.receive_pending();

        let port_config = self
            .ports
            .get_mut(port)
            .ok_or_else(|| StreamError::Link(format!("Unknown input port: {}", port)))?;

        let payload = match port_config.read_mode {
            ReadMode::SkipToLatest => port_config.mailbox.pop_latest(),
            ReadMode::ReadNextInOrder => port_config.mailbox.pop(),
        }
        .ok_or_else(|| StreamError::Link(format!("No data available on port: {}", port)))?;

        rmp_serde::from_slice(payload.data())
            .map_err(|e| StreamError::Link(format!("Failed to deserialize frame: {}", e)))
    }

    /// Check if a port has any payloads available.
    ///
    /// This first receives any pending data from the iceoryx2 Subscriber.
    pub fn has_data(&mut self, port: &str) -> bool {
        self.receive_pending();
        self.ports
            .get(port)
            .map(|p| !p.mailbox.is_empty())
            .unwrap_or(false)
    }

    /// Drain all payloads from the given port's mailbox.
    pub fn drain(&mut self, port: &str) -> impl Iterator<Item = FramePayload> + '_ {
        self.ports
            .get_mut(port)
            .into_iter()
            .flat_map(|p| p.mailbox.drain())
    }

    /// Route a payload to the appropriate mailbox based on its port_key.
    ///
    /// Returns true if the payload was routed, false if no matching mailbox exists.
    pub fn route(&mut self, payload: FramePayload) -> bool {
        let port = payload.port();
        if let Some(port_config) = self.ports.get_mut(port) {
            port_config.mailbox.push(payload);
            true
        } else {
            false
        }
    }

    /// Get the list of configured port names.
    pub fn port_names(&self) -> impl Iterator<Item = &str> {
        self.ports.keys().map(|s| s.as_str())
    }
}

impl Default for InputMailboxes {
    fn default() -> Self {
        Self::new()
    }
}
