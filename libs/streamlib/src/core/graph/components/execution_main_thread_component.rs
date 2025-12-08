// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use serde_json::Value as JsonValue;

use super::JsonComponent;

/// Processors that must run on main thread (Apple frameworks).
pub struct ExecutionMainThreadComponent;

impl JsonComponent for ExecutionMainThreadComponent {
    fn json_key(&self) -> &'static str {
        "execution_main_thread"
    }

    fn to_json(&self) -> JsonValue {
        serde_json::json!(true)
    }
}
