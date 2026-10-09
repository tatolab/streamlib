// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::sync::Arc;
use std::time::Instant;

use crate::core::processors::NodeTypesOneStreamResolves;

use super::edges::Link;
use super::nodes::ProcessorNode;
use petgraph::graph::DiGraph;

use super::traversal::{TraversalSource, TraversalSourceMut};
use crate::core::graph::{ExposedOutputPortsComponent, GraphNodeWithComponents};
use crate::core::json_schema::{
    ExposedOutputPortOutput, GraphResponse, LinkOutput, NodeNamesByProcessorId, ProcessorNodeOutput,
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

    /// The node types this graph's stream resolves.
    node_types_this_stream_resolves: Arc<NodeTypesOneStreamResolves>,

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
    /// Create a new empty Graph that resolves the natively compiled node types alone.
    pub fn new() -> Self {
        Self::new_resolving_node_types_through(Arc::new(NodeTypesOneStreamResolves::new()))
    }

    /// Create a new empty Graph that resolves node types through its stream's lookup.
    pub fn new_resolving_node_types_through(
        node_types_this_stream_resolves: Arc<NodeTypesOneStreamResolves>,
    ) -> Self {
        Self {
            digraph: DiGraph::new(),
            loaded_stream_name: None,
            node_types_this_stream_resolves,
            compiled_at: None,
            state: GraphState::Idle,
        }
    }

    /// The node types this graph's stream resolves.
    pub fn node_types_this_stream_resolves(&self) -> &Arc<NodeTypesOneStreamResolves> {
        &self.node_types_this_stream_resolves
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
        TraversalSourceMut::new(&mut self.digraph, &self.node_types_this_stream_resolves)
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
    /// Render this graph as the `/api/graph` payload.
    pub(crate) fn to_graph_response(&self, runtime_name: String) -> GraphResponse {
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
                        .flat_map(ExposedOutputPortsComponent::exposed_ports_and_their_levels)
                        .map(|(port, level)| ExposedOutputPortOutput {
                            node: node.display_name.clone(),
                            port: port.to_string(),
                            level,
                        })
                })
                .collect(),
            runtime_name,
        }
    }
}

/// The node `node_name` names once cast, refused by name — listing the nodes
/// `graph` holds — when there is none.
pub(crate) fn node_named_or_refused<'graph>(
    graph: &'graph Graph,
    node_name: &str,
) -> crate::core::Result<&'graph crate::core::graph::ProcessorNode> {
    graph
        .traversal()
        .v_with_node_name(node_name)
        .first()
        .ok_or_else(|| {
            crate::core::Error::ProcessorNotFound(format!(
                "no node in this stream is named {node_name:?}. This stream holds: {}",
                node_names_listed_for_a_refusal(
                    graph
                        .traversal()
                        .v(())
                        .iter()
                        .map(|node| node.display_name.as_str())
                )
            ))
        })
}

/// Port names, comma-joined in the order a node declares them for a refusal
/// that lists them — `none` when it declares none.
pub(crate) fn port_names_listed_for_a_refusal<'name>(
    port_names: impl IntoIterator<Item = &'name str>,
) -> String {
    let listed: Vec<&str> = port_names.into_iter().collect();
    if listed.is_empty() {
        "none".to_string()
    } else {
        listed.join(", ")
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
