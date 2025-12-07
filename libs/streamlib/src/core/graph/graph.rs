// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Unified Graph combining topology with embedded component storage.
//!
//! Graph is the public API for the processor pipeline graph. It internally manages:
//! - [`InternalProcessorLinkGraph`] - the petgraph-based processor/link topology
//!
//! Components are stored directly in node/link weights via TypeMap.
//! All access goes through `query()` for reads or `query_mut()` for mutations.

use std::time::Instant;

use serde::Serialize;

use super::internal::{GraphChecksum, InternalProcessorLinkGraph};
use super::nodes::ProcessorNode;
use crate::core::error::Result;

use crate::core::graph::IntoLinkPortRef;
use crate::core::links::LinkId;
use crate::core::processors::Processor;

use super::query::{QueryBuilder, QueryBuilderMut};

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
/// - `graph.query_mut()` for mutations
pub struct Graph {
    /// Internal topology store for processor nodes and link edges.
    graph: InternalProcessorLinkGraph,

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
            graph: InternalProcessorLinkGraph::new(),
            compiled_at: None,
            source_checksum: None,
            state: GraphState::Idle,
        }
    }

    // =========================================================================
    // Query Interface
    // =========================================================================

    /// Start a read-only query on the graph.
    pub fn query(&self) -> QueryBuilder<'_> {
        QueryBuilder::new(self)
    }

    /// Start a mutable query on the graph.
    pub fn query_mut(&mut self) -> QueryBuilderMut<'_> {
        QueryBuilderMut::new(self)
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
        self.source_checksum = Some(self.graph.checksum());
    }

    /// Check if recompilation is needed.
    pub fn needs_recompile(&self) -> bool {
        match self.source_checksum {
            Some(checksum) => self.graph.checksum() != checksum,
            None => true, // Never compiled
        }
    }

    // =========================================================================
    // Topology Mutation Operations
    // =========================================================================

    /// Add a processor node using its type and config.
    pub fn add_node<P>(&mut self, config: P::Config) -> Result<&mut ProcessorNode>
    where
        P: Processor + 'static,
        P::Config: serde::Serialize,
    {
        let node = self.graph.add_processor_node::<P>(config)?;
        let id = node.id.clone();
        Ok(self.graph.get_processor_mut(&id).unwrap())
    }

    /// Remove a processor from the graph.
    pub fn remove_processor(&mut self, id: impl AsRef<str>) {
        self.graph.remove_processor(id);
    }

    /// Add a link between two ports. Returns the link ID.
    pub fn add_edge(
        &mut self,
        from: impl IntoLinkPortRef,
        to: impl IntoLinkPortRef,
    ) -> Result<LinkId> {
        self.graph.add_link(from, to)
    }

    /// Remove a link from the graph.
    pub fn remove_link(&mut self, id: &LinkId) {
        self.graph.remove_link(id);
    }

    /// Update a processor's configuration.
    pub fn update_processor_config(
        &mut self,
        processor_id: impl AsRef<str>,
        config: serde_json::Value,
    ) -> Result<()> {
        self.graph.update_processor_config(processor_id, config)?;
        Ok(())
    }

    /// Validate the graph structure.
    pub fn validate(&self) -> Result<()> {
        self.graph.validate()
    }

    pub(crate) fn internal_graph(&self) -> &InternalProcessorLinkGraph {
        &self.graph
    }

    pub(crate) fn internal_graph_mut(&mut self) -> &mut InternalProcessorLinkGraph {
        &mut self.graph
    }

    /// Export graph as JSON.
    pub fn to_json(&self) -> serde_json::Value {
        self.graph.to_json()
    }

    /// Export graph as Graphviz DOT format.
    pub fn to_dot(&self) -> String {
        self.graph.to_dot()
    }
}

impl Serialize for Graph {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        self.to_json().serialize(serializer)
    }
}

impl std::fmt::Debug for Graph {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.graph.to_json())
    }
}

impl std::fmt::Display for Graph {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.graph.to_json())
    }
}
