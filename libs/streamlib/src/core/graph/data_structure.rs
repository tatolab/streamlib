// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::time::Instant;

use petgraph::graph::DiGraph;
use serde::Serialize;

use super::edges::Link;
use super::nodes::ProcessorNode;

use super::traversal::TraversalSource;

/// Graph state.
#[derive(Default, Clone, Copy, PartialEq, Eq, Debug)]
pub enum GraphState {
    #[default]
    Idle,
    Running,
    Paused,
    Stopping,
}

/// Checksum of a graph's structure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct GraphChecksum(pub u64);

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

    /// Checksum of the source graph at compile time.
    source_checksum: Option<GraphChecksum>,

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
            source_checksum: None,
            state: GraphState::Idle,
        }
    }

    // =========================================================================
    // Query Interface
    // =========================================================================

    /// Start a read-only query on the graph.
    pub fn traversal(&self) -> TraversalSource<'_> {
        TraversalSource::new(&self.digraph)
    }

    // =========================================================================
    // Direct DiGraph Access (for query builders)
    // =========================================================================

    /// Get read access to the underlying DiGraph.
    pub(crate) fn digraph(&self) -> &DiGraph<ProcessorNode, Link> {
        &self.digraph
    }

    /// Get mutable access to the underlying DiGraph.
    pub(crate) fn digraph_mut(&mut self) -> &mut DiGraph<ProcessorNode, Link> {
        &mut self.digraph
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

    /// Mark as compiled with current checksum.
    pub fn mark_compiled(&mut self) {
        self.compiled_at = Some(Instant::now());
        // TODO: implement checksum
        // self.source_checksum = Some(self.checksum());
    }

    /// Check if recompilation is needed.
    pub fn needs_recompile(&self) -> bool {
        // TODO: implement checksum comparison
        true
    }
}

impl Serialize for Graph {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        // TODO: implement proper serialization
        serde_json::json!({
            "nodes": [],
            "links": []
        })
        .serialize(serializer)
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
