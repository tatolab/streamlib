// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use crossbeam_channel::{Receiver, Sender};
use serde_json::Value as JsonValue;

use super::JsonComponent;

/// Channel to signal processor shutdown.
pub struct ShutdownChannelComponent {
    pub sender: Sender<()>,
    pub receiver: Option<Receiver<()>>,
}

impl ShutdownChannelComponent {
    /// Create a new shutdown channel.
    pub fn new() -> Self {
        let (sender, receiver) = crossbeam_channel::bounded(1);
        Self {
            sender,
            receiver: Some(receiver),
        }
    }

    /// Take the receiver (can only be done once).
    pub fn take_receiver(&mut self) -> Option<Receiver<()>> {
        self.receiver.take()
    }
}

impl Default for ShutdownChannelComponent {
    fn default() -> Self {
        Self::new()
    }
}

impl JsonComponent for ShutdownChannelComponent {
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
