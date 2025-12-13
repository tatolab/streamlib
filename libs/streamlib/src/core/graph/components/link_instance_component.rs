// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use serde_json::Value as JsonValue;

use super::JsonSerializableComponent;
use crate::core::links::BoxedLinkInstance;

/// ECS component storing the link instance (ring buffer ownership).
///
/// When this component is removed from an entity, the ring buffer is dropped
/// and all handles (data writers/readers) gracefully degrade.
pub struct LinkInstanceComponent(pub BoxedLinkInstance);

impl JsonSerializableComponent for LinkInstanceComponent {
    fn json_key(&self) -> &'static str {
        "buffer"
    }

    fn to_json(&self) -> JsonValue {
        serde_json::json!({
            "fill_level": self.0.len(),
            "is_empty": self.0.is_empty(),
            "has_data": self.0.has_data(),
            "strong_refs": self.0.strong_count(),
            "weak_refs": self.0.weak_count()
        })
    }
}
