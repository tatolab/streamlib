// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Trait hierarchy for graph weights with embedded component storage.
use crate::core::graph::components::Component;

/// Base trait for all graph weights (nodes and edges).
pub trait GraphWeight {
    /// Get the unique identifier for this weight.
    fn id(&self) -> &str;
}

/// Trait for node weights (processors) with component storage.
pub trait GraphNode: GraphWeight {
    /// Insert a component, replacing any existing component of the same type.
    fn insert<C: Component>(&mut self, component: C);

    /// Get an immutable reference to a component.
    fn get<C: Component>(&self) -> Option<&C>;

    /// Get a mutable reference to a component.
    fn get_mut<C: Component>(&mut self) -> Option<&mut C>;

    /// Remove a component and return it.
    fn remove<C: Component>(&mut self) -> Option<C>;

    /// Check if a component of the given type exists.
    fn has<C: Component>(&self) -> bool;
}

/// Trait for edge weights (links) with component storage.
pub trait GraphEdge: GraphWeight {
    /// Insert a component, replacing any existing component of the same type.
    fn insert<C: Component>(&mut self, component: C);

    /// Get an immutable reference to a component.
    fn get<C: Component>(&self) -> Option<&C>;

    /// Get a mutable reference to a component.
    fn get_mut<C: Component>(&mut self) -> Option<&mut C>;

    /// Remove a component and return it.
    fn remove<C: Component>(&mut self) -> Option<C>;

    /// Check if a component of the given type exists.
    fn has<C: Component>(&self) -> bool;
}
