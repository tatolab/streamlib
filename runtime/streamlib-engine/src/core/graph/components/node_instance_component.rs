// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::sync::Arc;

use parking_lot::Mutex;
use serde_json::Value as JsonValue;

use super::JsonSerializableComponent;
use crate::core::nodes::NodeInstance;

pub struct NodeInstanceComponent(pub Arc<Mutex<NodeInstance>>);

impl JsonSerializableComponent for NodeInstanceComponent {
    fn json_key(&self) -> &'static str {
        "node_instance"
    }

    fn to_json(&self) -> JsonValue {
        serde_json::json!({
            "attached": true,
            "arc_strong_count": Arc::strong_count(&self.0),
            "arc_weak_count": Arc::weak_count(&self.0)
        })
    }
}
