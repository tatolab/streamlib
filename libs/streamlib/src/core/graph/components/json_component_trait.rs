// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use serde_json::Value as JsonValue;

pub trait JsonComponent {
    /// The component's key in the JSON output.
    fn json_key(&self) -> &'static str;

    /// Serialize this component to JSON.
    fn to_json(&self) -> JsonValue;
}
