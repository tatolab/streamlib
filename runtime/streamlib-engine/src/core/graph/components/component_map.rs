// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use anymap2::Map;
use serde_json::Value as JsonValue;

use crate::core::JsonSerializableComponent;

/// Anything a node's component map can hold.
///
/// Storage and rendering are separate concerns and this is the storage half: a
/// component the control plane reads through something other than a
/// `components` key of its own — the settled `match_device` contracts, which
/// render on the ports that settled them — is storable without owing a
/// rendering it would never use.
pub trait StorableComponent: anymap2::any::Any + Send + Sync + 'static {}

impl<T: anymap2::any::Any + Send + Sync + 'static> StorableComponent for T {}

/// A component that also renders as its own key in `graph`.
pub trait Component: StorableComponent + JsonSerializableComponent {}

impl<T: StorableComponent + JsonSerializableComponent> Component for T {}

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
