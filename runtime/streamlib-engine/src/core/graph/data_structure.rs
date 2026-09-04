// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::time::Instant;

use super::edges::Link;
use super::nodes::ProcessorNode;
use petgraph::graph::DiGraph;

use super::traversal::{TraversalSource, TraversalSourceMut};
use crate::core::json_schema::{
    GraphResponse, LinkOutput, LoadedCapabilityExtensionOutput, ProcessorNodeOutput,
};

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
    ///
    /// Returns true if the graph has never been compiled.
    /// Note: This does not track modifications after compilation - callers should
    /// call `mark_compiled()` after successful compilation and ensure this is
    /// called before making changes, or always recompile after modifications.
    pub fn needs_recompile(&self) -> bool {
        self.compiled_at.is_none()
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

impl Graph {
    /// Render this graph as the `/api/graph` payload, carrying
    /// `loaded_capability_extensions` alongside it.
    ///
    /// The extensions are a property of the process, not of the graph, so the
    /// runtime that reads the registry passes them in.
    pub(crate) fn to_graph_response(
        &self,
        loaded_capability_extensions: Vec<LoadedCapabilityExtensionOutput>,
    ) -> GraphResponse {
        GraphResponse {
            nodes: self
                .digraph
                .node_indices()
                .map(|idx| ProcessorNodeOutput::from(&self.digraph[idx]))
                .collect(),
            links: self
                .digraph
                .edge_indices()
                .map(|idx| LinkOutput::from(&self.digraph[idx]))
                .collect(),
            extensions: loaded_capability_extensions,
        }
    }
}
