// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Instant;

use crate::core::context::StreamClockSpec;
use crate::core::processors::NodeTypesOneStreamResolves;

use super::edges::Link;
use super::nodes::ProcessorNode;
use petgraph::graph::DiGraph;

use super::traversal::{TraversalSource, TraversalSourceMut};
use crate::core::json_schema::{
    GraphResponse, LinkOutput, NodeNamesByProcessorId, ProcessorNodeOutput,
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

    /// The id of the stream this graph was loaded as, until then `None`.
    loaded_stream_id: Option<String>,

    /// The UUID of the stream asset whose graph document this graph was loaded from.
    stream_asset: Option<uuid::Uuid>,
    /// The name the stream this graph was loaded as is shown under.
    stream_display_name: Option<String>,

    /// The clock the stream this graph was loaded as runs on.
    stream_clock_spec: StreamClockSpec,

    /// The node types this graph's stream resolves.
    node_types_this_stream_resolves: Arc<NodeTypesOneStreamResolves>,

    /// The cast names of the nodes the loaded graph drew that the runtime does
    /// not run, which a node added live cannot take.
    node_names_the_graph_only_nodes_hold: HashSet<String>,

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
            loaded_stream_id: None,
            stream_asset: None,
            stream_display_name: None,
            stream_clock_spec: StreamClockSpec::Real,
            node_types_this_stream_resolves,
            node_names_the_graph_only_nodes_hold: HashSet::new(),
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

    /// The petgraph digraph itself, for a test of what reads it whole.
    #[cfg(test)]
    pub(crate) fn petgraph_digraph_for_a_test(&self) -> &DiGraph<ProcessorNode, Link> {
        &self.digraph
    }

    /// Start a mutable traversal on the graph.
    pub fn traversal_mut(&mut self) -> TraversalSourceMut<'_> {
        TraversalSourceMut::new(
            &mut self.digraph,
            &self.node_types_this_stream_resolves,
            &self.node_names_the_graph_only_nodes_hold,
        )
    }

    /// Record the cast names of the nodes the loaded graph drew that the
    /// runtime does not run, so no node added live takes one.
    pub fn set_node_names_the_graph_only_nodes_hold(
        &mut self,
        node_names_the_graph_only_nodes_hold: HashSet<String>,
    ) {
        self.node_names_the_graph_only_nodes_hold = node_names_the_graph_only_nodes_hold;
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

    /// The id of the stream this graph was loaded as, if it was loaded as one.
    pub fn loaded_stream_id(&self) -> Option<&str> {
        self.loaded_stream_id.as_deref()
    }

    /// Record the id of the stream this graph was loaded as.
    pub fn set_loaded_stream_id(&mut self, stream_id: String) {
        self.loaded_stream_id = Some(stream_id);
    }

    /// The UUID of the stream asset whose graph document this graph was loaded from.
    pub fn stream_asset(&self) -> Option<uuid::Uuid> {
        self.stream_asset
    }

    /// Record the UUID of the stream asset whose graph document this graph was loaded from.
    pub fn set_stream_asset(&mut self, stream_asset: Option<uuid::Uuid>) {
        self.stream_asset = stream_asset;
    }

    /// Record the name the stream this graph was loaded as is shown under.
    pub fn set_stream_display_name(&mut self, display_name: String) {
        self.stream_display_name = Some(display_name);
    }

    /// The clock the stream this graph was loaded as runs on.
    pub fn stream_clock_spec(&self) -> StreamClockSpec {
        self.stream_clock_spec
    }

    /// Record the clock the stream this graph was loaded as runs on.
    pub fn set_stream_clock_spec(&mut self, stream_clock_spec: StreamClockSpec) {
        self.stream_clock_spec = stream_clock_spec;
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
    /// Render this graph as the `/api/graph` payload, on the machine named
    /// `machine`.
    pub(crate) fn to_graph_response(&self, machine: String) -> GraphResponse {
        let node_names = NodeNamesByProcessorId::of(self.digraph.node_weights());
        GraphResponse {
            asset: self
                .stream_asset
                .map(|stream_asset| stream_asset.to_string()),
            display_name: self.stream_display_name.clone(),
            stream: self.loaded_stream_id.clone(),
            clock: self.stream_clock_spec,
            nodes: self
                .digraph
                .node_indices()
                .map(|idx| &self.digraph[idx])
                .map(|node| ProcessorNodeOutput {
                    inputs_waiting_for_a_value: crate::core::compiler::compiler_ops::input_port_pin_values::the_inputs_a_function_node_has_no_value_for(self, node),
                    ..ProcessorNodeOutput::from(node)
                })
                .collect(),
            links: self
                .digraph
                .edge_indices()
                .map(|idx| &self.digraph[idx])
                .map(|link| LinkOutput::of_a_link(link, &node_names))
                .collect(),
            references: Vec::new(),
            machine,
            groups: Vec::new(),
        }
    }
}

/// The node `node_name` names once cast in `graph`, the graph of the stream
/// `stream_id`, refused by name — listing the nodes it holds — when there is
/// none.
pub(crate) fn node_named_or_refused<'graph>(
    graph: &'graph Graph,
    stream_id: &str,
    node_name: &str,
) -> crate::core::Result<&'graph crate::core::graph::ProcessorNode> {
    graph
        .traversal()
        .v_with_node_name(node_name)
        .first()
        .ok_or_else(|| {
            crate::core::Error::ProcessorNotFound(format!(
                "no node in the stream `{stream_id}` is named {node_name:?}. The stream \
                 `{stream_id}` holds: {}",
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

/// Names, comma-joined in the order given for a refusal that lists them —
/// `when_empty` when there are none.
pub(crate) fn names_listed_for_a_refusal(
    names: impl IntoIterator<Item = impl AsRef<str>>,
    when_empty: &str,
) -> String {
    let listed: Vec<String> = names
        .into_iter()
        .map(|name| name.as_ref().to_string())
        .collect();
    if listed.is_empty() {
        when_empty.to_string()
    } else {
        listed.join(", ")
    }
}

/// Port names, comma-joined in the order a node declares them for a refusal
/// that lists them — `none` when it declares none.
pub(crate) fn port_names_listed_for_a_refusal<'name>(
    port_names: impl IntoIterator<Item = &'name str>,
) -> String {
    names_listed_for_a_refusal(port_names, "none")
}

/// Node names, sorted and comma-joined for a refusal that lists what a graph
/// holds — `no node` when it holds none.
pub(crate) fn node_names_listed_for_a_refusal<'name>(
    node_names: impl IntoIterator<Item = &'name str>,
) -> String {
    let mut sorted: Vec<&str> = node_names.into_iter().collect();
    sorted.sort_unstable();
    names_listed_for_a_refusal(sorted, "no node")
}
