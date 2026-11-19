// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use crossbeam_channel::{Receiver, Sender};
use streamlib_runtime_client_contract::local_api_wire_contract::NodeReadyBarrierOnTheWire;

use super::GraphRenders;
use crate::core::json_schema::GraphNodeComponentsOnTheWire;

pub struct NodeReadyBarrierComponent {
    ready_sender: Option<Sender<()>>,
    continue_receiver: Option<Receiver<()>>,
}

/// Handle for the compiler to wait for READY and signal CONTINUE.
pub struct NodeReadyBarrierHandle {
    pub ready_receiver: Receiver<()>,
    pub continue_sender: Sender<()>,
}

impl NodeReadyBarrierComponent {
    /// Create a new barrier, returning the component and the compiler-side handle.
    pub fn new() -> (Self, NodeReadyBarrierHandle) {
        let (ready_sender, ready_receiver) = crossbeam_channel::bounded(1);
        let (continue_sender, continue_receiver) = crossbeam_channel::bounded(1);

        let component = Self {
            ready_sender: Some(ready_sender),
            continue_receiver: Some(continue_receiver),
        };

        let handle = NodeReadyBarrierHandle {
            ready_receiver,
            continue_sender,
        };

        (component, handle)
    }

    pub fn signal_ready(&mut self) {
        if let Some(sender) = self.ready_sender.take() {
            let _ = sender.send(());
        }
    }

    /// Wait for the compiler's CONTINUE: whether it came, or `false` when the compiler dropped
    /// its handle instead, abandoning a compile that failed before its wiring completed.
    #[must_use]
    pub fn wait_for_continue(&mut self) -> bool {
        match self.continue_receiver.take() {
            Some(receiver) => receiver.recv().is_ok(),
            None => true,
        }
    }
}

impl Default for NodeReadyBarrierComponent {
    fn default() -> Self {
        Self::new().0
    }
}

impl GraphRenders<GraphNodeComponentsOnTheWire> for NodeReadyBarrierComponent {
    fn render_onto(&self, components: &mut GraphNodeComponentsOnTheWire) {
        components.ready_barrier = Some(NodeReadyBarrierOnTheWire {
            ready_signaled: self.ready_sender.is_none(),
            continue_received: self.continue_receiver.is_none(),
        });
    }
}
