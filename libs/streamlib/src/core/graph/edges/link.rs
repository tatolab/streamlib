// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use super::super::components::default_components;
use crate::core::graph::ComponentMap;
use serde::{Deserialize, Serialize};

use super::super::LinkUniqueId;
use super::super::{GraphEdge, GraphWeight};
use super::LinkCapacity;
use super::{LinkEndpoint, LinkState};

/// Link in the processor graph (connection between two ports) with embedded component storage.
#[derive(Debug, Serialize, Deserialize)]
pub struct Link {
    /// Unique link identifier
    pub id: LinkUniqueId,
    /// Source endpoint (output port)
    pub source: LinkEndpoint,
    /// Target endpoint (input port)
    pub target: LinkEndpoint,
    /// Ring buffer capacity for the channel.
    #[serde(default)]
    pub capacity: LinkCapacity,
    /// Current state of the link.
    #[serde(default)]
    pub state: LinkState,
    /// Runtime components (not serialized).
    #[serde(skip, default = "default_components")]
    components: ComponentMap,
}

impl PartialEq for Link {
    fn eq(&self, other: &Self) -> bool {
        // Compare only static fields, not runtime components
        self.id == other.id
            && self.source == other.source
            && self.target == other.target
            && self.capacity == other.capacity
            && self.state == other.state
    }
}

impl Eq for Link {}

impl Link {
    /// Create a new link from port addresses with default capacity. ID is generated automatically.
    pub fn new(from_port: &str, to_port: &str) -> Self {
        Self::with_capacity(from_port, to_port, LinkCapacity::default())
    }

    /// Create a new link with explicit buffer capacity. ID is generated automatically using cuid2.
    pub fn with_capacity(from_port: &str, to_port: &str, capacity: LinkCapacity) -> Self {
        let (source_node, source_port) = from_port.split_once('.').unwrap_or((from_port, ""));
        let (target_node, target_port) = to_port.split_once('.').unwrap_or((to_port, ""));

        Self {
            id: LinkUniqueId::new(),
            source: LinkEndpoint::source(source_node, source_port),
            target: LinkEndpoint::target(target_node, target_port),
            capacity,
            state: LinkState::Pending,
            components: ComponentMap::new(),
        }
    }

    /// Set the link state.
    pub fn set_state(&mut self, state: LinkState) {
        self.state = state;
    }

    /// Get source port address in "node.port" format
    pub fn from_port(&self) -> String {
        self.source.to_address()
    }

    /// Get target port address in "node.port" format
    pub fn to_port(&self) -> String {
        self.target.to_address()
    }
}

impl GraphWeight for Link {
    fn id(&self) -> &str {
        &self.id
    }
}

impl GraphEdge for Link {
    fn insert<C: Send + Sync + 'static>(&mut self, component: C) {
        self.components.insert(component);
    }

    fn get<C: Send + Sync + 'static>(&self) -> Option<&C> {
        self.components.get::<C>()
    }

    fn get_mut<C: Send + Sync + 'static>(&mut self) -> Option<&mut C> {
        self.components.get_mut::<C>()
    }

    fn remove<C: Send + Sync + 'static>(&mut self) -> Option<C> {
        self.components.remove::<C>()
    }

    fn has<C: Send + Sync + 'static>(&self) -> bool {
        self.components.contains::<C>()
    }
}
