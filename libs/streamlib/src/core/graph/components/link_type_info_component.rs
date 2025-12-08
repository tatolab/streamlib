// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::any::TypeId;

use serde_json::Value as JsonValue;

use crate::core::graph::LinkCapacity;

use super::JsonComponent;

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
    pub fn new<T: 'static>(capacity: LinkCapacity) -> Self {
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
