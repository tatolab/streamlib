// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use serde_json::Value as JsonValue;

use super::JsonSerializableComponent;

pub struct PendingDeletionComponent;

impl JsonSerializableComponent for PendingDeletionComponent {
    fn json_key(&self) -> &'static str {
        "pending_deletion"
    }

    fn to_json(&self) -> JsonValue {
        serde_json::json!(true)
    }
}
