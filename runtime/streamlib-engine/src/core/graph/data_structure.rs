// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::time::Instant;

use crate::core::error::Result;

use super::edges::Link;
use super::nodes::ProcessorNode;
use petgraph::graph::DiGraph;

use super::traversal::{TraversalSource, TraversalSourceMut};
use crate::core::graph::{ExposedOutputPortsComponent, GraphNodeWithComponents};
use crate::core::json_schema::{
    ExposedOutputPortOutput, GraphResponse, LinkOutput, LoadedCapabilityExtensionOutput,
    NodeNamesByProcessorId, ProcessorNodeOutput, RuntimeMeshOutput,
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

    /// The name of the stream this graph was loaded as, until then `None`.
    loaded_stream_name: Option<String>,

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
            loaded_stream_name: None,
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

    /// `requested_name` cast, refused by name when a node already has it.
    pub(crate) fn the_requested_node_name_unless_taken(
        &self,
        requested_name: &str,
    ) -> Result<String> {
        super::traversal::the_requested_node_name_unless_taken(&self.digraph, requested_name)
    }

    /// The name of the stream this graph was loaded as, if it was loaded as one.
    pub fn loaded_stream_name(&self) -> Option<&str> {
        self.loaded_stream_name.as_deref()
    }

    /// Record the name of the stream this graph was loaded as.
    pub fn set_loaded_stream_name(&mut self, stream_name: String) {
        self.loaded_stream_name = Some(stream_name);
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
    /// `loaded_capability_extensions` and `runtime_mesh` alongside it.
    ///
    /// Neither is a property of the graph — the extensions belong to the
    /// process and the mesh to the runtime — so the runtime that reads them
    /// passes them in.
    pub(crate) fn to_graph_response(
        &self,
        loaded_capability_extensions: Vec<LoadedCapabilityExtensionOutput>,
        runtime_mesh: RuntimeMeshOutput,
    ) -> GraphResponse {
        let node_names = NodeNamesByProcessorId::of(self.digraph.node_weights());
        GraphResponse {
            stream: self.loaded_stream_name.clone(),
            nodes: self
                .digraph
                .node_indices()
                .map(|idx| ProcessorNodeOutput::from(&self.digraph[idx]))
                .collect(),
            links: self
                .digraph
                .edge_indices()
                .map(|idx| &self.digraph[idx])
                .map(|link| LinkOutput::of_a_link(link, &node_names))
                .collect(),
            exposed: self
                .digraph
                .node_weights()
                .flat_map(|node| {
                    node.get::<ExposedOutputPortsComponent>()
                        .into_iter()
                        .flat_map(|exposed| exposed.0.iter())
                        .map(|port| ExposedOutputPortOutput {
                            node: node.display_name.clone(),
                            port: port.clone(),
                        })
                })
                .collect(),
            extensions: loaded_capability_extensions,
            mesh: runtime_mesh,
        }
    }
}

/// Node names, sorted and comma-joined for a refusal that lists what a graph
/// holds — `no node` when it holds none.
pub(crate) fn node_names_listed_for_a_refusal<'name>(
    node_names: impl IntoIterator<Item = &'name str>,
) -> String {
    let mut sorted: Vec<&str> = node_names.into_iter().collect();
    sorted.sort_unstable();
    if sorted.is_empty() {
        "no node".to_string()
    } else {
        sorted.join(", ")
    }
}
