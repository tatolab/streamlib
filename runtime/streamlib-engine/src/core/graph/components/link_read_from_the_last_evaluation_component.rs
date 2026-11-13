// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use serde_json::Value as JsonValue;

use super::JsonSerializableComponent;

/// Marks a link between two function nodes that closes a loop of them: what it
/// feeds reads its value as it was at the last evaluation, 0 at the first.
/// `graph` renders it as `"reads_the_last_evaluation": true`.
pub struct LinkReadFromTheLastEvaluationComponent;

impl JsonSerializableComponent for LinkReadFromTheLastEvaluationComponent {
    fn json_key(&self) -> &'static str {
        "reads_the_last_evaluation"
    }

    fn to_json(&self) -> JsonValue {
        JsonValue::Bool(true)
    }
}
