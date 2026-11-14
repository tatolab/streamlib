// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use serde_json::Value as JsonValue;

use super::JsonSerializableComponent;
use crate::core::graph::edges::LinkState;

pub struct LinkStateComponent(pub LinkState);

impl Default for LinkStateComponent {
    fn default() -> Self {
        Self(LinkState::Pending)
    }
}

impl JsonSerializableComponent for LinkStateComponent {
    fn json_key(&self) -> &'static str {
        "state"
    }

    fn to_json(&self) -> JsonValue {
        serde_json::json!(format!("{:?}", self.0))
    }
}
