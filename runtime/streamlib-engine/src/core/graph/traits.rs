// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Trait hierarchy for graph weights with embedded component storage.

use crate::core::graph::components::{
    ComponentMap, ComponentSerializer, LinkComponent, NodeComponent, StorableComponent,
};
use crate::core::json_schema::{GraphLinkComponentsOnTheWire, GraphNodeComponentsOnTheWire};

/// Base trait for all graph weights (nodes and edges).
pub trait GraphWeight {
    /// Get the unique identifier for this weight.
    fn id(&self) -> &str;
}

pub trait GraphNodeWithComponents: GraphWeight {
    /// Access the component map.
    fn components(&self) -> &ComponentMap;

    /// Access the component map mutably.
    fn components_mut(&mut self) -> &mut ComponentMap;

    /// Access the component serializers.
    fn component_serializers(&self) -> &[ComponentSerializer<GraphNodeComponentsOnTheWire>];

    /// Access the component serializers mutably.
    fn component_serializers_mut(
        &mut self,
    ) -> &mut Vec<ComponentSerializer<GraphNodeComponentsOnTheWire>>;

    /// Insert a component, replacing any existing component of the same type.
    fn insert<C: NodeComponent>(&mut self, component: C) {
        self.components_mut().insert(component);
        self.component_serializers_mut()
            .push(Box::new(|map, rendered| {
                if let Some(component) = map.get::<C>() {
                    component.render_onto(rendered);
                }
            }));
    }

    /// Insert a component that the control plane reads through something other
    /// than a `components` key of its own, replacing any existing component of
    /// the same type.
    ///
    /// [`Self::insert`] registers a serializer alongside every component, which
    /// is right for the ones `graph` renders as themselves. A component whose
    /// content already reaches a reader somewhere else — the settled
    /// `match_device` contracts, which render on the ports that settled them —
    /// would otherwise appear twice, and two renderings of one fact is one
    /// rendering too many to keep in agreement.
    fn insert_component_without_rendering_it<C: StorableComponent>(&mut self, component: C) {
        self.components_mut().insert(component);
    }

    /// Get an immutable reference to a component.
    fn get<C: StorableComponent>(&self) -> Option<&C> {
        self.components().get::<C>()
    }

    /// Get a mutable reference to a component.
    fn get_mut<C: StorableComponent>(&mut self) -> Option<&mut C> {
        self.components_mut().get_mut::<C>()
    }

    /// Remove a component and return it.
    fn remove<C: StorableComponent>(&mut self) -> Option<C> {
        self.components_mut().remove::<C>()
    }

    /// Check if a component of the given type exists.
    fn has<C: StorableComponent>(&self) -> bool {
        self.components().contains::<C>()
    }

    /// Every component `graph` renders, each under its own key.
    fn serialize_components(&self) -> GraphNodeComponentsOnTheWire {
        let mut rendered = GraphNodeComponentsOnTheWire::default();
        for serializer in self.component_serializers() {
            serializer(self.components(), &mut rendered);
        }
        rendered
    }
}

/// Trait for edge weights (links) with component storage.
pub trait GraphEdgeWithComponents: GraphWeight {
    /// Access the component map.
    fn components(&self) -> &ComponentMap;

    /// Access the component map mutably.
    fn components_mut(&mut self) -> &mut ComponentMap;

    /// Access the component serializers.
    fn component_serializers(&self) -> &[ComponentSerializer<GraphLinkComponentsOnTheWire>];

    /// Access the component serializers mutably.
    fn component_serializers_mut(
        &mut self,
    ) -> &mut Vec<ComponentSerializer<GraphLinkComponentsOnTheWire>>;

    /// Insert a component, replacing any existing component of the same type.
    fn insert<C: LinkComponent>(&mut self, component: C) {
        self.components_mut().insert(component);
        self.component_serializers_mut()
            .push(Box::new(|map, rendered| {
                if let Some(component) = map.get::<C>() {
                    component.render_onto(rendered);
                }
            }));
    }

    /// Insert a component `graph` never renders, replacing any existing
    /// component of the same type.
    fn insert_component_without_rendering_it<C: StorableComponent>(&mut self, component: C) {
        self.components_mut().insert(component);
    }

    /// Get an immutable reference to a component.
    fn get<C: StorableComponent>(&self) -> Option<&C> {
        self.components().get::<C>()
    }

    /// Get a mutable reference to a component.
    fn get_mut<C: StorableComponent>(&mut self) -> Option<&mut C> {
        self.components_mut().get_mut::<C>()
    }

    /// Remove a component and return it.
    fn remove<C: StorableComponent>(&mut self) -> Option<C> {
        self.components_mut().remove::<C>()
    }

    /// Check if a component of the given type exists.
    fn has<C: StorableComponent>(&self) -> bool {
        self.components().contains::<C>()
    }

    /// Every component `graph` renders, each under its own key.
    fn serialize_components(&self) -> GraphLinkComponentsOnTheWire {
        let mut rendered = GraphLinkComponentsOnTheWire::default();
        for serializer in self.component_serializers() {
            serializer(self.components(), &mut rendered);
        }
        rendered
    }
}
