// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Component for tracking subprocess bridge state on graph nodes.

use std::sync::Arc;

use crossbeam_channel::{bounded, Receiver, Sender};
use parking_lot::Mutex;
use serde_json::Value as JsonValue;

use super::JsonSerializableComponent;

/// State of the subprocess bridge connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubprocessBridgeState {
    /// This processor doesn't use subprocess bridging.
    NotApplicable,
    /// Subprocess spawned, waiting for it to register endpoint with broker.
    AwaitingSubprocessRegistration,
    /// Host connecting to subprocess endpoint.
    Connecting,
    /// Connection established, waiting for subprocess to send bridge_ready.
    AwaitingBridgeReady,
    /// Bridge fully established, ready for frame transfer.
    Connected,
    /// Connection failed with an error message.
    Failed(String),
}

impl Default for SubprocessBridgeState {
    fn default() -> Self {
        Self::NotApplicable
    }
}

/// Component for tracking subprocess bridge state on a processor node.
///
/// This component allows other parts of the system to query whether a
/// processor's subprocess bridge is ready for frame transfer.
pub struct SubprocessBridgeComponent {
    /// Current state of the bridge.
    pub state: Arc<Mutex<SubprocessBridgeState>>,
    /// Channel for signaling when bridge becomes ready (internal use).
    bridge_ready_sender: Option<Sender<Result<(), String>>>,
    /// Channel for waiting on bridge ready (internal use).
    bridge_ready_receiver: Option<Receiver<Result<(), String>>>,
}

/// Handle for waiting on bridge readiness from outside the processor thread.
pub struct SubprocessBridgeReadyHandle {
    pub receiver: Receiver<Result<(), String>>,
}

impl SubprocessBridgeComponent {
    /// Create a new component with initial state.
    pub fn new(initial_state: SubprocessBridgeState) -> Self {
        let (tx, rx) = bounded(1);
        Self {
            state: Arc::new(Mutex::new(initial_state)),
            bridge_ready_sender: Some(tx),
            bridge_ready_receiver: Some(rx),
        }
    }

    /// Create a component that indicates subprocess bridging is not applicable.
    pub fn not_applicable() -> Self {
        Self {
            state: Arc::new(Mutex::new(SubprocessBridgeState::NotApplicable)),
            bridge_ready_sender: None,
            bridge_ready_receiver: None,
        }
    }

    /// Create a component and return a handle for waiting on readiness.
    pub fn new_with_handle(
        initial_state: SubprocessBridgeState,
    ) -> (Self, SubprocessBridgeReadyHandle) {
        let (tx, rx) = bounded(1);
        let component = Self {
            state: Arc::new(Mutex::new(initial_state)),
            bridge_ready_sender: Some(tx),
            bridge_ready_receiver: None, // Handle takes the receiver
        };
        let handle = SubprocessBridgeReadyHandle { receiver: rx };
        (component, handle)
    }

    /// Update the state.
    pub fn set_state(&self, new_state: SubprocessBridgeState) {
        let mut state = self.state.lock();
        *state = new_state;
    }

    /// Get the current state.
    pub fn get_state(&self) -> SubprocessBridgeState {
        self.state.lock().clone()
    }

    /// Check if the bridge is connected and ready.
    pub fn is_connected(&self) -> bool {
        matches!(self.get_state(), SubprocessBridgeState::Connected)
    }

    /// Check if the bridge has failed.
    pub fn is_failed(&self) -> bool {
        matches!(self.get_state(), SubprocessBridgeState::Failed(_))
    }

    /// Signal that the bridge is ready (success).
    /// Called by the processor thread when bridge_ready is received.
    pub fn signal_ready(&mut self) {
        self.set_state(SubprocessBridgeState::Connected);
        if let Some(tx) = self.bridge_ready_sender.take() {
            let _ = tx.send(Ok(()));
        }
    }

    /// Signal that the bridge failed.
    /// Called by the processor thread when bridge setup fails.
    pub fn signal_failed(&mut self, error: String) {
        self.set_state(SubprocessBridgeState::Failed(error.clone()));
        if let Some(tx) = self.bridge_ready_sender.take() {
            let _ = tx.send(Err(error));
        }
    }

    /// Wait for the bridge to become ready (blocking).
    /// Returns Ok(()) if connected, Err with message if failed.
    pub fn wait_for_ready(&mut self) -> Result<(), String> {
        if let Some(rx) = self.bridge_ready_receiver.take() {
            rx.recv()
                .map_err(|_| "Bridge ready channel disconnected".to_string())?
        } else {
            // Check current state
            match self.get_state() {
                SubprocessBridgeState::Connected => Ok(()),
                SubprocessBridgeState::Failed(e) => Err(e),
                SubprocessBridgeState::NotApplicable => Ok(()),
                other => Err(format!("Unexpected bridge state: {:?}", other)),
            }
        }
    }
}

impl Default for SubprocessBridgeComponent {
    fn default() -> Self {
        Self::not_applicable()
    }
}

impl JsonSerializableComponent for SubprocessBridgeComponent {
    fn json_key(&self) -> &'static str {
        "subprocess_bridge"
    }

    fn to_json(&self) -> JsonValue {
        let state = self.state.lock();
        let state_str = match &*state {
            SubprocessBridgeState::NotApplicable => "not_applicable",
            SubprocessBridgeState::AwaitingSubprocessRegistration => {
                "awaiting_subprocess_registration"
            }
            SubprocessBridgeState::Connecting => "connecting",
            SubprocessBridgeState::AwaitingBridgeReady => "awaiting_bridge_ready",
            SubprocessBridgeState::Connected => "connected",
            SubprocessBridgeState::Failed(_) => "failed",
        };

        let mut json = serde_json::json!({
            "state": state_str,
        });

        if let SubprocessBridgeState::Failed(ref error) = *state {
            json["error"] = serde_json::json!(error);
        }

        json
    }
}
