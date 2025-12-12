// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::sync::Arc;

use parking_lot::Mutex;
use serde_json::Value as JsonValue;

use super::JsonSerializableComponent;
use crate::core::processors::ProcessorState;

/// Current state of the processor.
pub struct StateComponent(pub Arc<Mutex<ProcessorState>>);

impl Default for StateComponent {
    fn default() -> Self {
        Self(Arc::new(Mutex::new(ProcessorState::Idle)))
    }
}

impl JsonSerializableComponent for StateComponent {
    fn json_key(&self) -> &'static str {
        "state"
    }

    fn to_json(&self) -> JsonValue {
        let state = self.0.lock();
        serde_json::json!(format!("{:?}", *state))
    }
}
