// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use crossbeam_channel::{Receiver, Sender};
use serde_json::Value as JsonValue;

use super::JsonSerializableComponent;

/// Synchronization barrier for processor startup.
///
/// Used to coordinate between the compiler and processor threads:
/// 1. Compiler spawns processor thread
/// 2. Processor thread creates instance, attaches to graph
/// 3. Processor thread signals READY via this barrier
/// 4. Compiler waits for READY, then wires ring buffers
/// 5. Compiler signals CONTINUE via this barrier
/// 6. Processor thread continues with setup() and process loop
pub struct ProcessorReadyBarrierComponent {
    ready_sender: Option<Sender<()>>,
    continue_receiver: Option<Receiver<()>>,
}

/// Handle for the compiler to wait for READY and signal CONTINUE.
pub struct ProcessorReadyBarrierHandle {
    pub ready_receiver: Receiver<()>,
    pub continue_sender: Sender<()>,
}

impl ProcessorReadyBarrierComponent {
    /// Create a new barrier, returning the component and the compiler-side handle.
    pub fn new() -> (Self, ProcessorReadyBarrierHandle) {
        let (ready_sender, ready_receiver) = crossbeam_channel::bounded(1);
        let (continue_sender, continue_receiver) = crossbeam_channel::bounded(1);

        let component = Self {
            ready_sender: Some(ready_sender),
            continue_receiver: Some(continue_receiver),
        };

        let handle = ProcessorReadyBarrierHandle {
            ready_receiver,
            continue_sender,
        };

        (component, handle)
    }

    /// Signal that the processor is ready (instance created and attached).
    /// Called by the processor thread.
    pub fn signal_ready(&mut self) {
        if let Some(sender) = self.ready_sender.take() {
            let _ = sender.send(());
        }
    }

    /// Wait for the compiler to signal that wiring is complete.
    /// Called by the processor thread.
    pub fn wait_for_continue(&mut self) {
        if let Some(receiver) = self.continue_receiver.take() {
            let _ = receiver.recv();
        }
    }
}

impl Default for ProcessorReadyBarrierComponent {
    fn default() -> Self {
        Self::new().0
    }
}

impl JsonSerializableComponent for ProcessorReadyBarrierComponent {
    fn json_key(&self) -> &'static str {
        "ready_barrier"
    }

    fn to_json(&self) -> JsonValue {
        serde_json::json!({
            "ready_signaled": self.ready_sender.is_none(),
            "continue_received": self.continue_receiver.is_none()
        })
    }
}
