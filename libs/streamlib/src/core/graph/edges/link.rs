// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use anymap2::Map;
use serde::{Deserialize, Serialize};

use super::super::{GraphEdge, GraphWeight, ProcessorId};
use crate::core::links::graph::link_id::__private::new_unchecked as new_link_id;
use crate::core::links::LinkId;

/// State of a link in the graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum LinkState {
    /// Link exists in graph but not yet wired (pending commit).
    #[default]
    Pending,
    /// Link is actively wired with a ring buffer channel.
    Wired,
    /// Link is being disconnected.
    Disconnecting,
    /// Link was disconnected (will be removed from graph).
    Disconnected,
    /// Link is in error state (wiring failed).
    Error,
}

impl std::fmt::Display for LinkState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Pending => write!(f, "Pending"),
            Self::Wired => write!(f, "Wired"),
            Self::Disconnecting => write!(f, "Disconnecting"),
            Self::Disconnected => write!(f, "Disconnected"),
            Self::Error => write!(f, "Error"),
        }
    }
}

/// Direction of a port in a link endpoint
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LinkDirection {
    /// Input port (receives data)
    Input,
    /// Output port (sends data)
    Output,
}

/// One endpoint of a link (source or target)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkEndpoint {
    /// Node (processor) ID
    pub node: ProcessorId,
    /// Port name on the node
    pub port: String,
    /// Direction of this port
    pub direction: LinkDirection,
}

impl LinkEndpoint {
    /// Create a new source endpoint (output port)
    pub fn source(node: impl Into<ProcessorId>, port: impl Into<String>) -> Self {
        Self {
            node: node.into(),
            port: port.into(),
            direction: LinkDirection::Output,
        }
    }

    /// Create a new target endpoint (input port)
    pub fn target(node: impl Into<ProcessorId>, port: impl Into<String>) -> Self {
        Self {
            node: node.into(),
            port: port.into(),
            direction: LinkDirection::Input,
        }
    }

    /// Convert to port address format "node.port"
    pub fn to_address(&self) -> String {
        format!("{}.{}", self.node, self.port)
    }
}

/// Default ring buffer capacity for links.
pub const DEFAULT_LINK_CAPACITY: usize = 4;

/// TypeMap for component storage (Send + Sync).
type ComponentMap = Map<dyn anymap2::any::Any + Send + Sync>;

fn default_capacity() -> usize {
    DEFAULT_LINK_CAPACITY
}

fn default_components() -> ComponentMap {
    ComponentMap::new()
}

/// Link in the processor graph (connection between two ports) with embedded component storage.
#[derive(Debug, Serialize, Deserialize)]
pub struct Link {
    /// Unique link identifier
    pub id: LinkId,
    /// Source endpoint (output port)
    pub source: LinkEndpoint,
    /// Target endpoint (input port)
    pub target: LinkEndpoint,
    /// Ring buffer capacity for the channel.
    #[serde(default = "default_capacity")]
    pub capacity: usize,
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
        Self::with_capacity(from_port, to_port, DEFAULT_LINK_CAPACITY)
    }

    /// Create a new link with explicit buffer capacity. ID is generated automatically using cuid2.
    pub fn with_capacity(from_port: &str, to_port: &str, capacity: usize) -> Self {
        let (source_node, source_port) = from_port.split_once('.').unwrap_or((from_port, ""));
        let (target_node, target_port) = to_port.split_once('.').unwrap_or((to_port, ""));

        Self {
            id: new_link_id(cuid2::create_id()),
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
