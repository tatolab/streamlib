// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use serde_json::Value as JsonValue;

use super::JsonSerializableComponent;

/// Processors using Rayon work-stealing pool.
pub struct ExecutionRayonPoolComponent;

impl JsonSerializableComponent for ExecutionRayonPoolComponent {
    fn json_key(&self) -> &'static str {
        "execution_rayon_pool"
    }

    fn to_json(&self) -> JsonValue {
        serde_json::json!(true)
    }
}
