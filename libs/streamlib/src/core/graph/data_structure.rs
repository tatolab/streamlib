// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::time::Instant;

use super::edges::Link;
use super::nodes::ProcessorNode;
use super::{GraphEdgeWithComponents, GraphNodeWithComponents};
use petgraph::graph::DiGraph;

use serde::ser::SerializeStruct;
use serde::Serialize;

use super::traversal::{TraversalSource, TraversalSourceMut};

/// Graph state.
#[derive(Default, Clone, Copy, PartialEq, Eq, Debug)]
pub enum GraphState {
    #[default]
    Idle,
    Running,
    Paused,
    Stopping,
}

/// Unified graph with topology and embedded component storage.
///
/// All access goes through the query interface:
/// - `graph.query()` for read operations
/// - `graph.query()` for mutations
pub struct Graph {
    /// The petgraph DiGraph storing processors as nodes and links as edges.
    digraph: DiGraph<ProcessorNode, Link>,

    /// When the graph was last compiled.
    compiled_at: Option<Instant>,

    /// Graph-level state.
    state: GraphState,
}

impl Default for Graph {
    fn default() -> Self {
        Self::new()
    }
}

impl Graph {
    /// Create a new empty Graph.
    pub fn new() -> Self {
        Self {
            digraph: DiGraph::new(),
            compiled_at: None,
            state: GraphState::Idle,
        }
    }

    // =========================================================================
    // Query Interface
    // =========================================================================

    /// Start a traversal on the graph.
    pub fn traversal(&self) -> TraversalSource<'_> {
        TraversalSource::new(&self.digraph)
    }

    /// Start a mutable traversal on the graph.
    pub fn traversal_mut(&mut self) -> TraversalSourceMut<'_> {
        TraversalSourceMut::new(&mut self.digraph)
    }

    // =========================================================================
    // Graph State
    // =========================================================================

    /// Get the current state.
    pub fn state(&self) -> GraphState {
        self.state
    }

    /// Set the graph state.
    pub fn set_state(&mut self, state: GraphState) {
        self.state = state;
    }

    /// Get when the graph was compiled.
    pub fn compiled_at(&self) -> Option<Instant> {
        self.compiled_at
    }

    /// Mark as compiled.
    pub fn mark_compiled(&mut self) {
        self.compiled_at = Some(Instant::now());
    }

    /// Check if recompilation is needed.
    pub fn needs_recompile(&self) -> bool {
        // TODO: implement checksum-based change detection
        true
    }
}

impl std::fmt::Debug for Graph {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Graph {{ nodes: {}, edges: {} }}",
            self.digraph.node_count(),
            self.digraph.edge_count()
        )
    }
}

impl std::fmt::Display for Graph {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Graph({} processors, {} links)",
            self.digraph.node_count(),
            self.digraph.edge_count()
        )
    }
}

/// Helper for serializing a ProcessorNode with its components.
struct SerializableNode<'a>(&'a ProcessorNode);

impl Serialize for SerializableNode<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let node = self.0;
        let components = node.serialize_components();

        let mut state = serializer.serialize_struct("ProcessorNode", 6)?;
        state.serialize_field("id", &node.id)?;
        state.serialize_field("type", &node.processor_type)?;
        state.serialize_field("config", &node.config)?;
        state.serialize_field("config_checksum", &node.config_checksum)?;
        state.serialize_field("ports", &node.ports)?;
        state.serialize_field("components", &components)?;
        state.end()
    }
}

/// Helper for serializing a Link with its components.
struct SerializableLink<'a>(&'a Link);

impl Serialize for SerializableLink<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let link = self.0;
        let components = link.serialize_components();

        let mut state = serializer.serialize_struct("Link", 6)?;
        state.serialize_field("id", &link.id)?;
        state.serialize_field("source", &link.source)?;
        state.serialize_field("target", &link.target)?;
        state.serialize_field("capacity", &link.capacity)?;
        state.serialize_field("state", &link.state)?;
        state.serialize_field("components", &components)?;
        state.end()
    }
}

impl Serialize for Graph {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let nodes: Vec<_> = self
            .digraph
            .node_indices()
            .map(|idx| SerializableNode(&self.digraph[idx]))
            .collect();
        let links: Vec<_> = self
            .digraph
            .edge_indices()
            .map(|idx| SerializableLink(&self.digraph[idx]))
            .collect();

        let mut state = serializer.serialize_struct("Graph", 2)?;
        state.serialize_field("nodes", &nodes)?;
        state.serialize_field("links", &links)?;
        state.end()
    }
}
