// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Trait hierarchy for graph weights with embedded component storage.

use serde_json::Value as JsonValue;

use crate::core::graph::components::{Component, ComponentMap, ComponentSerializer};

/// Base trait for all graph weights (nodes and edges).
pub trait GraphWeight {
    /// Get the unique identifier for this weight.
    fn id(&self) -> &str;
}

/// Trait for node weights (processors) with component storage.
pub trait GraphNodeWithComponents: GraphWeight {
    /// Access the component map.
    fn components(&self) -> &ComponentMap;

    /// Access the component map mutably.
    fn components_mut(&mut self) -> &mut ComponentMap;

    /// Access the component serializers.
    fn component_serializers(&self) -> &[ComponentSerializer];

    /// Access the component serializers mutably.
    fn component_serializers_mut(&mut self) -> &mut Vec<ComponentSerializer>;

    /// Insert a component, replacing any existing component of the same type.
    fn insert<C: Component>(&mut self, component: C) {
        self.components_mut().insert(component);
        self.component_serializers_mut().push(Box::new(|map| {
            map.get::<C>()
                .map(|c| (c.json_key().to_string(), c.to_json()))
        }));
    }

    /// Get an immutable reference to a component.
    fn get<C: Component>(&self) -> Option<&C> {
        self.components().get::<C>()
    }

    /// Get a mutable reference to a component.
    fn get_mut<C: Component>(&mut self) -> Option<&mut C> {
        self.components_mut().get_mut::<C>()
    }

    /// Remove a component and return it.
    fn remove<C: Component>(&mut self) -> Option<C> {
        self.components_mut().remove::<C>()
    }

    /// Check if a component of the given type exists.
    fn has<C: Component>(&self) -> bool {
        self.components().contains::<C>()
    }

    /// Serialize all components to a JSON map.
    fn serialize_components(&self) -> serde_json::Map<String, JsonValue> {
        let mut map = serde_json::Map::new();
        for serializer in self.component_serializers() {
            if let Some((key, value)) = serializer(self.components()) {
                map.insert(key, value);
            }
        }
        map
    }
}

/// Trait for edge weights (links) with component storage.
pub trait GraphEdgeWithComponents: GraphWeight {
    /// Access the component map.
    fn components(&self) -> &ComponentMap;

    /// Access the component map mutably.
    fn components_mut(&mut self) -> &mut ComponentMap;

    /// Access the component serializers.
    fn component_serializers(&self) -> &[ComponentSerializer];

    /// Access the component serializers mutably.
    fn component_serializers_mut(&mut self) -> &mut Vec<ComponentSerializer>;

    /// Insert a component, replacing any existing component of the same type.
    fn insert<C: Component>(&mut self, component: C) {
        self.components_mut().insert(component);
        self.component_serializers_mut().push(Box::new(|map| {
            map.get::<C>()
                .map(|c| (c.json_key().to_string(), c.to_json()))
        }));
    }

    /// Get an immutable reference to a component.
    fn get<C: Component>(&self) -> Option<&C> {
        self.components().get::<C>()
    }

    /// Get a mutable reference to a component.
    fn get_mut<C: Component>(&mut self) -> Option<&mut C> {
        self.components_mut().get_mut::<C>()
    }

    /// Remove a component and return it.
    fn remove<C: Component>(&mut self) -> Option<C> {
        self.components_mut().remove::<C>()
    }

    /// Check if a component of the given type exists.
    fn has<C: Component>(&self) -> bool {
        self.components().contains::<C>()
    }

    /// Serialize all components to a JSON map.
    fn serialize_components(&self) -> serde_json::Map<String, JsonValue> {
        let mut map = serde_json::Map::new();
        for serializer in self.component_serializers() {
            if let Some((key, value)) = serializer(self.components()) {
                map.insert(key, value);
            }
        }
        map
    }
}
