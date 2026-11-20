// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Instant;

use crate::core::context::StreamClockSpec;
use crate::core::nodes::NodeTypesOneStreamResolves;

use super::edges::Link;
use super::nodes::{GraphNode, NodeRunId};
use super::{GraphEdgeWithComponents, PendingDeletionComponent};
use petgraph::graph::DiGraph;

use super::traversal::{TraversalSource, TraversalSourceMut};
use crate::core::PortDirection;
use crate::core::compiler::compiler_ops::function_node_pool::{
    is_the_function_node_pool_type, the_links_carried_through_the_function_node_pool_of,
};
use crate::core::compiler::compiler_ops::input_port_pin_values::the_inputs_a_function_node_has_no_value_for;
use crate::core::graph::input_values::{
    LiveInputValuesOfAStream, ValuesWiresCarryIntoSettings, the_value_an_input_reads,
};
use crate::core::graph::pin_type_link_checks::the_verdict_on_a_link_between_typed_ends;
use crate::core::graph::the_pin_a_port_resolves_to;
use crate::core::json_schema::{
    GraphNodeOutput, GraphResponse, LinkOutput, NodeNamesByNodeRunId, rendered_link,
    rendered_link_carried_through_the_function_node_pool,
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

    /// The values the Value wires into this graph's settings read at setup carry, held apart
    /// from its nodes and kept across a Stop.
    values_wires_carry_into_settings: Arc<ValuesWiresCarryIntoSettings>,
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
    /// Give the node `node_run_id` the slots its pin groups hold by its links and its values;
    /// answers whether a port came or went.
    pub(crate) fn reshape_the_pin_group_slots_of_the_node(
        &mut self,
        node_run_id: &NodeRunId,
    ) -> bool {
        self.digraph
            .node_indices()
            .find(|node_index| self.digraph[*node_index].id == *node_run_id)
            .is_some_and(|node_index| {
                super::nodes::reshape_the_pin_group_slots_of(&mut self.digraph, node_index)
            })
    }

    /// Grow the pin group of the node `node_run_id` holding port `port_name` of `direction` to
    /// hold it, when the slot it names is past the group's last: a link into it is judged with
    /// the port there.
    pub(crate) fn grow_the_pin_group_holding_the_port(
        &mut self,
        node_run_id: &NodeRunId,
        direction: PortDirection,
        port_name: &str,
    ) {
        if let Some(node_index) = self
            .digraph
            .node_indices()
            .find(|node_index| self.digraph[*node_index].id == *node_run_id)
        {
            super::nodes::grow_a_pin_group_to_hold_the_port(
                &mut self.digraph,
                node_index,
                direction,
                port_name,
            );
        }
    }

    /// Grow the pin group holding each of `from` and `to`, as
    /// [`Self::grow_the_pin_group_holding_the_port`] does, for a link between them to be judged.
    pub(crate) fn grow_the_pin_groups_holding_the_ends(
        &mut self,
        from: &super::OutputLinkPortRef,
        to: &super::InputLinkPortRef,
    ) {
        self.grow_the_pin_group_holding_the_port(
            from.node_run_id(),
            PortDirection::Output,
            from.port_name(),
        );
        self.grow_the_pin_group_holding_the_port(
            to.node_run_id(),
            PortDirection::Input,
            to.port_name(),
        );
    }

    /// Give the nodes of `from` and `to` the slots their links and values hold, a slot grown for
    /// a link not made given back.
    pub(crate) fn reshape_the_pin_groups_holding_the_ends(
        &mut self,
        from: &super::OutputLinkPortRef,
        to: &super::InputLinkPortRef,
    ) {
        self.reshape_the_pin_group_slots_of_the_node(from.node_run_id());
        self.reshape_the_pin_group_slots_of_the_node(to.node_run_id());
    }

    /// Refuse `value` as the count of the node `node_run_id`'s pin group named `input` when it
    /// leaves out a slot a link reaches.
    pub(crate) fn refuse_a_slot_count_leaving_out_a_linked_slot(
        &self,
        node_run_id: &NodeRunId,
        input: &str,
        value: Option<&serde_json::Value>,
    ) -> crate::core::Result<()> {
        match self
            .digraph
            .node_indices()
            .find(|node_index| self.digraph[*node_index].id == *node_run_id)
        {
            Some(node_index) => super::nodes::refuse_a_slot_count_leaving_out_a_linked_slot(
                &self.digraph,
                node_index,
                input,
                value,
            ),
            None => Ok(()),
        }
    }

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
            values_wires_carry_into_settings: Arc::default(),
        }
    }

    /// The values set live on this graph's inputs, which change under its read lock.
    pub fn live_input_values(&self) -> &Arc<LiveInputValuesOfAStream> {
        &self.live_input_values
    }

    /// The values the Value wires into this graph's settings read at setup carry, which change
    /// under its read lock.
    pub(crate) fn values_wires_carry_into_settings(&self) -> &Arc<ValuesWiresCarryIntoSettings> {
        &self.values_wires_carry_into_settings
    }

    /// Whether input `input` of the node `node_run_id` is a setting its node reads at setup;
    /// `false` when this graph holds no such node or input.
    pub(crate) fn is_a_setting_read_at_setup(&self, node_run_id: &NodeRunId, input: &str) -> bool {
        self.traversal().v(node_run_id).first().is_some_and(|node| {
            node.input_port(input)
                .is_some_and(super::PortInfo::is_a_setting_read_at_setup)
        })
    }

    /// Each `GraphInput` of this graph, each at its stream's top level.
    pub fn every_graph_input(&self) -> impl Iterator<Item = &GraphNode> {
        self.digraph
            .node_weights()
            .filter(|node| node.is_a_graph_input())
    }

    /// The node whose UUID is `node_uuid`.
    pub fn node_by_uuid(&self, node_uuid: uuid::Uuid) -> Option<&GraphNode> {
        self.digraph
            .node_weights()
            .find(|node| node.node_uuid == node_uuid)
    }

    /// Whether `link` stands: neither it nor a node it joins is pending deletion, so an edit
    /// judges the graph as it is once what is pending deletion leaves.
    pub fn stands(&self, link: &Link) -> bool {
        let the_node_stands = |node_run_id: &NodeRunId| {
            self.node_of_the_run_id(node_run_id)
                .is_none_or(GraphNode::stands)
        };
        !link.has::<PendingDeletionComponent>()
            && the_node_stands(link.source().node_run_id())
            && the_node_stands(link.to_port().node_run_id())
    }

    /// The link feeding the whole of input `input` of the node `node_run_id`.
    pub fn link_into(&self, node_run_id: &NodeRunId, input: &str) -> Option<&Link> {
        self.link_into_the_input_or_its_member(node_run_id, input, None)
    }

    /// The link feeding input `input` of the node `node_run_id` at `member`, or the whole input
    /// when `member` is `None`.
    pub fn link_into_the_input_or_its_member(
        &self,
        node_run_id: &NodeRunId,
        input: &str,
        member: Option<&str>,
    ) -> Option<&Link> {
        self.digraph.edge_weights().find(|link| {
            let to = link.to_port();
            to.node_run_id() == node_run_id && to.port_name() == input && to.member() == member
        })
    }

    /// `from`, a link's output end, under its port's current name; an end whose node or port
    /// the graph does not hold is left as given, for its refusal to name.
    pub fn output_under_its_current_name(
        &self,
        from: super::OutputLinkPortRef,
    ) -> super::OutputLinkPortRef {
        let member = from.member().map(str::to_owned);
        self.node_of_the_run_id(from.node_run_id())
            .and_then(|node| node.try_output(from.port_name()))
            .map(|current| current.at_member(member))
            .unwrap_or(from)
    }

    /// `to`, a link's input end, under its port's current name; an end whose node or port the
    /// graph does not hold is left as given, for its refusal to name.
    pub fn input_under_its_current_name(
        &self,
        to: super::InputLinkPortRef,
    ) -> super::InputLinkPortRef {
        let member = to.member().map(str::to_owned);
        self.node_of_the_run_id(to.node_run_id())
            .and_then(|node| node.try_input(to.port_name()))
            .map(|current| current.at_member(member))
            .unwrap_or(to)
    }

    /// The node `node_run_id` names, if the graph holds it.
    fn node_of_the_run_id(&self, node_run_id: &NodeRunId) -> Option<&GraphNode> {
        self.digraph
            .node_weights()
            .find(|node| &node.id == node_run_id)
    }

    /// Drop the node `node_run_id` and the links into and out of it, then forget the values set
    /// live on its inputs as `why` it leaves says; whether it was dropped. A node the graph
    /// still holds under the same UUID, one that replaced it while it was pending deletion,
    /// keeps the value set live on each input that takes the values the leaving node's did; an
    /// input whose declaration changed, such as its pin type, reads no value set on the one
    /// before.
    #[must_use]
    pub fn drop_the_node(&mut self, node_run_id: &NodeRunId, why: WhyTheNodeLeaves) -> bool {
        let leaving = self
            .traversal()
            .v(node_run_id)
            .first()
            .map(|node| (node.node_uuid, node.ports.inputs.clone()));
        let dropped = !self.traversal_mut().v(node_run_id).drop().exists();
        if dropped && let Some((node_uuid, inputs_of_the_node_leaving)) = leaving {
            let inputs_its_replacement_declares_alike: HashSet<String> = self
                .node_by_uuid(node_uuid)
                .into_iter()
                .flat_map(|replacement| replacement.ports.inputs.iter())
                .filter(|input| {
                    inputs_of_the_node_leaving
                        .iter()
                        .any(|leaving| leaving.takes_the_values_it_takes(input))
                })
                .map(|input| input.name.clone())
                .collect();
            self.live_input_values.forget_the_node_but_the_inputs(
                node_uuid,
                why,
                &inputs_its_replacement_declares_alike,
            );
            if why == WhyTheNodeLeaves::Removed && self.node_by_uuid(node_uuid).is_none() {
                self.values_wires_carry_into_settings
                    .forget_the_node(node_uuid);
            }
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

    /// The names the graph's graph-only nodes hold, which no node the graph runs may take.
    pub fn node_names_the_graph_only_nodes_hold(&self) -> &HashSet<String> {
        &self.node_names_the_graph_only_nodes_hold
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
        let fed_by_input = super::WhatTheLinksIntoAnInputFeed::of_each_input_of(self);
        let unfed = super::WhatTheLinksIntoAnInputFeed::default();
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
                    fed_by_input
                        .get(&(node.id.clone(), port.name.clone()))
                        .unwrap_or(&unfed),
                );
                rendered_input.pin =
                    the_pin_a_port_resolves_to(self, node, port, PortDirection::Input);
            }
            for (rendered_output, port) in
                rendered.ports.outputs.iter_mut().zip(&node.ports.outputs)
            {
                rendered_output.pin =
                    the_pin_a_port_resolves_to(self, node, port, PortDirection::Output);
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
            clock: (!self.stream_clock_spec.is_real()).then_some(self.stream_clock_spec),
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
            condition: None,
        }
    }

    /// Every link, in the order the graph holds them, each pair of links that
    /// carries one link through the function node pool rendered once as that
    /// link, where the link into the pool stands; each says what it converts
    /// and warns of as the judgement of its two typed ends does.
    fn links_rendered_as_given(&self, node_names: &NodeNamesByNodeRunId) -> Vec<LinkOutput> {
        let carried_links = the_links_carried_through_the_function_node_pool_of(self);
        let carried_link_wired_as = |link: &Link| {
            carried_links.iter().find(|carried| {
                carried.into_the_function_node_pool.id == link.id
                    || carried.out_of_the_function_node_pool.id == link.id
            })
        };
        let conversions = self.node_types_this_stream_resolves().pin_conversions();
        let verdict_on = |from: &Link, to: &Link| {
            the_verdict_on_a_link_between_typed_ends(self, &from.source, &to.target, &conversions)
        };
        self.digraph
            .edge_weights()
            .filter_map(|link| match carried_link_wired_as(link) {
                None => Some(rendered_link(
                    link,
                    node_names,
                    verdict_on(link, link).as_ref(),
                )),
                Some(carried) if carried.into_the_function_node_pool.id == link.id => {
                    Some(rendered_link_carried_through_the_function_node_pool(
                        carried,
                        node_names,
                        verdict_on(
                            carried.into_the_function_node_pool,
                            carried.out_of_the_function_node_pool,
                        )
                        .as_ref(),
                    ))
                }
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
