// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use anymap2::Map;
use serde_json::Value as JsonValue;

use crate::core::JsonSerializableComponent;

pub trait Component: anymap2::any::Any + JsonSerializableComponent + Send + Sync + 'static {}

impl<T: anymap2::any::Any + JsonSerializableComponent + Send + Sync + 'static> Component for T {}

/// TypeMap for component storage (Send + Sync).
pub type ComponentMap = Map<dyn anymap2::any::Any + Send + Sync>;

/// Closure that serializes a component from the map.
pub type ComponentSerializer =
    Box<dyn Fn(&ComponentMap) -> Option<(String, JsonValue)> + Send + Sync>;

pub fn default_components() -> ComponentMap {
    ComponentMap::new()
}

pub fn default_component_serializers() -> Vec<ComponentSerializer> {
    Vec::new()
}
