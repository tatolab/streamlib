// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::sync::Arc;

use parking_lot::Mutex;
use serde_json::Value as JsonValue;

use super::JsonSerializableComponent;
use crate::core::processors::BoxedProcessor;

/// The instantiated processor instance.
pub struct ProcessorInstanceComponent(pub Arc<Mutex<BoxedProcessor>>);

impl JsonSerializableComponent for ProcessorInstanceComponent {
    fn json_key(&self) -> &'static str {
        "processor_instance"
    }

    fn to_json(&self) -> JsonValue {
        serde_json::json!({
            "attached": true,
            "arc_strong_count": Arc::strong_count(&self.0),
            "arc_weak_count": Arc::weak_count(&self.0)
        })
    }
}
