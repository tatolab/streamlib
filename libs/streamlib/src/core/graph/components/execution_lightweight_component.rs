// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use serde_json::Value as JsonValue;

use super::JsonSerializableComponent;

/// Lightweight processors (no dedicated resources).
pub struct ExecutionLightweightComponent;

impl JsonSerializableComponent for ExecutionLightweightComponent {
    fn json_key(&self) -> &'static str {
        "execution_lightweight"
    }

    fn to_json(&self) -> JsonValue {
        serde_json::json!(true)
    }
}
