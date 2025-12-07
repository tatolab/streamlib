// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! ECS components for link runtime state.

use std::any::TypeId;

use serde_json::Value as JsonValue;

use super::super::runtime::BoxedLinkInstance;
use crate::core::graph::JsonComponent;

/// ECS component storing the link instance (ring buffer ownership).
///
/// When this component is removed from an entity, the ring buffer is dropped
/// and all handles (data writers/readers) gracefully degrade.
pub struct LinkInstanceComponent(pub BoxedLinkInstance);

impl JsonComponent for LinkInstanceComponent {
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

/// ECS component storing link type information for debugging and validation.
pub struct LinkTypeInfoComponent {
    /// TypeId of the message type flowing through this link.
    pub type_id: TypeId,
    /// Human-readable type name.
    pub type_name: &'static str,
    /// Ring buffer capacity.
    pub capacity: usize,
}

impl LinkTypeInfoComponent {
    /// Create new link type info.
    pub fn new<T: 'static>(capacity: usize) -> Self {
        Self {
            type_id: TypeId::of::<T>(),
            type_name: std::any::type_name::<T>(),
            capacity,
        }
    }
}

impl JsonComponent for LinkTypeInfoComponent {
    fn json_key(&self) -> &'static str {
        "type_info"
    }

    fn to_json(&self) -> JsonValue {
        serde_json::json!({
            "type_name": self.type_name,
            "capacity": self.capacity
        })
    }
}
