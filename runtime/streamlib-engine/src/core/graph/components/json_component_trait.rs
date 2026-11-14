// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use serde_json::Value as JsonValue;

pub trait JsonSerializableComponent {
    /// The component's key in the JSON output.
    fn json_key(&self) -> &'static str;

    /// Serialize this component to JSON.
    fn to_json(&self) -> JsonValue;
}
