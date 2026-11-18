// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Instant;

use crate::core::context::StreamClockSpec;
use crate::core::nodes::NodeTypesOneStreamResolves;

use super::edges::Link;
use super::nodes::{GraphNode, NodeRunId};
use petgraph::graph::DiGraph;

use super::traversal::{TraversalSource, TraversalSourceMut};
use crate::core::compiler::compiler_ops::function_node_pool::{
    is_the_function_node_pool_type, the_links_carried_through_the_function_node_pool_of,
};
use crate::core::compiler::compiler_ops::input_port_pin_values::the_inputs_a_function_node_has_no_value_for;
use crate::core::graph::input_values::{LiveInputValuesOfAStream, the_value_an_input_reads};
use crate::core::json_schema::{GraphNodeOutput, GraphResponse, LinkOutput, NodeNamesByNodeRunId};

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
    digraph: DiGraph<GraphNode, Link>,

    /// The id of the stream this graph was loaded as, until then `None`.
    loaded_stream_id: Option<String>,

    /// The UUID of the stream asset whose graph document this graph was loaded from.
    stream_asset: Option<uuid::Uuid>,
    /// The display name the graph document of the stream this graph was loaded as writes.
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

    /// The values set live on this graph's inputs, held apart from its nodes.
    live_input_values: Arc<LiveInputValuesOfAStream>,
}

/// Why a node leaves its stream's graph, which decides what the stream forgets of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WhyTheNodeLeaves {
    /// Removed from the graph: every value set live on its inputs is forgotten, and the
    /// project's store forgets each kept one.
    Removed,
    /// Torn down as its stream stops, to be put back as the graph held it: each value set live
    /// on a `reset` input is forgotten, and each on a `kept` input held across.
    TornDownAtAStop,
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
        Self::new_of_a_stream(
            node_types_this_stream_resolves,
            LiveInputValuesOfAStream::default(),
        )
    }

    /// Create a new empty Graph of a stream, resolving node types through its stream's lookup
    /// and holding `live_input_values`, empty, as the values set live on its inputs.
    pub(crate) fn new_of_a_stream(
        node_types_this_stream_resolves: Arc<NodeTypesOneStreamResolves>,
        live_input_values: LiveInputValuesOfAStream,
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
            live_input_values: Arc::new(live_input_values),
        }
    }

    /// The values set live on this graph's inputs, which change under its read lock.
    pub fn live_input_values(&self) -> &Arc<LiveInputValuesOfAStream> {
        &self.live_input_values
    }

    /// The node whose UUID is `node_uuid`.
    pub fn node_by_uuid(&self, node_uuid: uuid::Uuid) -> Option<&GraphNode> {
        self.digraph
            .node_weights()
            .find(|node| node.node_uuid == node_uuid)
    }

    /// The link feeding input `input` of the node `node_run_id`.
    pub fn link_into(&self, node_run_id: &NodeRunId, input: &str) -> Option<&Link> {
        self.digraph.edge_weights().find(|link| {
            link.to_port().node_run_id() == node_run_id && link.to_port().port_name() == input
        })
    }

    /// `from` and `to`, the ends of a link, each under its port's current name; an end whose
    /// node or port the graph does not hold is left as given, for its refusal to name.
    pub fn link_ends_under_their_current_port_names(
        &self,
        from: super::OutputLinkPortRef,
        to: super::InputLinkPortRef,
    ) -> (super::OutputLinkPortRef, super::InputLinkPortRef) {
        let node_of = |node_run_id: &NodeRunId| {
            self.digraph
                .node_weights()
                .find(|node| &node.id == node_run_id)
        };
        let from = node_of(from.node_run_id())
            .and_then(|node| node.try_output(from.port_name()))
            .unwrap_or(from);
        let to = node_of(to.node_run_id())
            .and_then(|node| node.try_input(to.port_name()))
            .unwrap_or(to);
        (from, to)
    }

    /// Drop the node `node_run_id` and the links into and out of it, then forget the values set
    /// live on its inputs as `why` it leaves says; whether it was dropped.
    #[must_use]
    pub fn drop_the_node(&mut self, node_run_id: &NodeRunId, why: WhyTheNodeLeaves) -> bool {
        let node_uuid = self
            .traversal()
            .v(node_run_id)
            .first()
            .map(|node| node.node_uuid);
        let dropped = !self.traversal_mut().v(node_run_id).drop().exists();
        if dropped && let Some(node_uuid) = node_uuid {
            self.live_input_values.forget_the_node(node_uuid, why);
        }
        dropped
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
    pub(crate) fn petgraph_digraph_for_a_test(&self) -> &DiGraph<GraphNode, Link> {
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

    /// Record the display name the graph document of the stream this graph was loaded as writes.
    pub fn set_stream_display_name(&mut self, display_name: Option<String>) {
        self.stream_display_name = display_name;
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
            "Graph({} nodes, {} links)",
            self.digraph.node_count(),
            self.digraph.edge_count()
        )
    }
}

impl Graph {
    /// Render this graph as the `/api/graph` payload, on the machine named
    /// `machine`: the graph as it was given, so the stream's function node pool
    /// renders under `function_node_pool` rather than as a node, and each link
    /// it carries renders once, as the link it was given.
    pub(crate) fn to_graph_response(&self, machine: String) -> GraphResponse {
        let node_names = NodeNamesByNodeRunId::of(self.digraph.node_weights());
        let linked_inputs: HashSet<(&NodeRunId, &str)> = self
            .digraph
            .edge_weights()
            .map(|link| (link.to_port().node_run_id(), link.to_port().port_name()))
            .collect();
        let render_a_node = |node: &GraphNode| {
            let mut rendered = GraphNodeOutput {
                inputs_waiting_for_a_value: the_inputs_a_function_node_has_no_value_for(self, node),
                ..GraphNodeOutput::from(node)
            };
            for (rendered_input, port) in rendered.ports.inputs.iter_mut().zip(&node.ports.inputs) {
                rendered_input.value_read = the_value_an_input_reads(
                    self,
                    node,
                    port,
                    linked_inputs.contains(&(&node.id, port.name.as_str())),
                );
            }
            rendered
        };
        let is_the_pool = |node: &&GraphNode| is_the_function_node_pool_type(&node.node_type);
        GraphResponse {
            asset: self
                .stream_asset
                .map(|stream_asset| stream_asset.to_string()),
            display_name: self.stream_display_name.clone(),
            stream: self.loaded_stream_id.clone(),
            clock: crate::core::json_schema::clock_rendered(self.stream_clock_spec),
            nodes: self
                .digraph
                .node_weights()
                .filter(|node| !is_the_pool(node))
                .map(render_a_node)
                .collect(),
            links: self.links_rendered_as_given(&node_names),
            references: Vec::new(),
            machine,
            groups: Vec::new(),
            studio: None,
            function_node_pool: self
                .digraph
                .node_weights()
                .find(is_the_pool)
                .map(render_a_node),
        }
    }

    /// Every link, in the order the graph holds them, each pair of links that
    /// carries one link through the function node pool rendered once as that
    /// link, where the link into the pool stands.
    fn links_rendered_as_given(&self, node_names: &NodeNamesByNodeRunId) -> Vec<LinkOutput> {
        let carried_links = the_links_carried_through_the_function_node_pool_of(self);
        let carried_link_wired_as = |link: &Link| {
            carried_links.iter().find(|carried| {
                carried.into_the_function_node_pool.id == link.id
                    || carried.out_of_the_function_node_pool.id == link.id
            })
        };
        self.digraph
            .edge_weights()
            .filter_map(|link| match carried_link_wired_as(link) {
                None => Some(LinkOutput::of_a_link(link, node_names)),
                Some(carried) if carried.into_the_function_node_pool.id == link.id => Some(
                    LinkOutput::of_a_link_carried_through_the_function_node_pool(
                        carried, node_names,
                    ),
                ),
                Some(_) => None,
            })
            .collect()
    }
}

/// The node `node_name` names once cast in `graph`, the graph of the stream
/// `stream_id`, refused by name — listing the nodes it holds — when there is
/// none.
pub(crate) fn node_named_or_refused<'graph>(
    graph: &'graph Graph,
    stream_id: &str,
    node_name: &str,
) -> crate::core::Result<&'graph crate::core::graph::GraphNode> {
    graph
        .traversal()
        .v_with_node_name(node_name)
        .first()
        .ok_or_else(|| {
            crate::core::Error::NodeNotFound(format!(
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
