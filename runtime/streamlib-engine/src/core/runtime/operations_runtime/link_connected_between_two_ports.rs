// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A link connected between an output port and an input port of one stream's graph, judged
//! before any channel exists, and the same judgement of a link made nowhere: between two ends
//! the graph holds, between one it holds and a port of a node type no node has yet, or among the
//! nodes of a placed stream's level.

use std::collections::HashMap;
use std::sync::Arc;

use streamlib_project_repository::{
    StreamGraphDocument, StreamGraphDocumentLink, StreamGraphDocumentNode,
};

use super::link_ends_this_graph_cannot_take::{
    refuse_a_destination_this_graph_cannot_take, refuse_a_source_this_graph_cannot_take,
};
use super::{LiveGraphChangeOnOneStream, announce_or_hold, commit_live_graph_change};
use crate::core::compiler::compiler_ops::function_node_island::{
    link_in, refuse_a_link_an_island_cannot_take,
    refuse_a_link_feeding_an_island_into_the_app_process,
    refuse_a_link_leaving_a_reactive_reader_fed_only_by_islands,
    refuse_the_links_through_a_node_not_yet_added,
};
use crate::core::compiler::compiler_ops::refuse_a_link_a_channel_cannot_carry;
use crate::core::compiler::{Compiler, PendingOperation};
use crate::core::descriptors::{
    DeclaredPinType, NodeClassImportPath, PinLinkOutcome, PinLinkVerdict, PinLinkWarning, PinType,
    PortDescriptor, ResolvedPin,
};
use crate::core::graph::legacy_config_of_a_node::{
    node_spec_holding_the_input_values, node_spec_of_a_document_node,
};
use crate::core::graph::{
    ALinkConnected, Graph, Link, LinkUniqueId, NodeRunId, WhyTheNodeLeaves,
    cast_exposed_name_to_url_safe, refuse_a_link_its_pin_types_or_families_refuse,
    refuse_the_names_and_codec_the_links_through_a_convert_node_refuse,
    the_verdict_on_a_link_between_typed_ends,
};
use crate::core::nodes::{NodeSpec, NodeTypesOneStreamResolves};
use crate::core::pubsub::RuntimeEvent;
use crate::core::runtime::WhatALiveConnectMakesOfALink;
use crate::core::runtime::streams_loaded_in_this_runtime::stream_graph_operation_applied_live::runs_as_no_node_of_its_own;
use crate::core::{Error, InputLinkPortRef, OutputLinkPortRef, PortDirection, Result};
use crate::iceoryx2::ChannelName;

/// What connect does with a link the engine cannot wire, and when it judges the rule a reader
/// that runs reactively sets: that something other than an island feeds it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::core::runtime) enum WhenTheEngineCannotWireALink {
    /// Refuse it, judged whole now, as an edit made on its own is.
    Refuse,
    /// Refuse it, the reader rule left to a sweep once each link of an added node or a batch of
    /// edits is made, so the order they are made in never matters.
    RefuseLeavingTheReaderRuleToASweep,
    /// Make nothing and answer why, so a saved graph opens and plays without the link, the
    /// reader rule left to the sweep a load or a reconcile makes once each link is made.
    SetAsideLeavingTheReaderRuleToASweep,
}

impl WhenTheEngineCannotWireALink {
    fn judges_the_reader_rule_now(self) -> bool {
        self == Self::Refuse
    }
}

/// What connect made of one link.
#[derive(Debug)]
pub(in crate::core::runtime) enum ALinkConnectedOrSetAside {
    /// The link, connected.
    Connected(LinkUniqueId),
    /// Nothing: the engine cannot wire the link, for this refusal.
    SetAside(Error),
}

impl ALinkConnectedOrSetAside {
    /// The link connected, or the refusal it was set aside for.
    pub(in crate::core::runtime) fn connected(self) -> Result<LinkUniqueId> {
        match self {
            Self::Connected(link_id) => Ok(link_id),
            Self::SetAside(refusal) => Err(refusal),
        }
    }
}

/// Core implementation for connect - takes owned Arcs for 'static lifetime.
///
/// Connect judges two typed ports by their pin link verdict, and any other two by their
/// families, names, codec and formats, and refuses, by name before any channel exists, a link
/// that does not connect as it is; `a_link_connected` says how leniently a link of a saved
/// graph is judged by its pin types. A link the engine cannot wire is refused or set aside, as
/// `when_the_engine_cannot_wire_it` says.
#[tracing::instrument(
    name = "stream.connect",
    skip(live_graph_change),
    fields(from = %from, to = %to),
)]
pub(in crate::core::runtime) async fn connect_impl(
    live_graph_change: LiveGraphChangeOnOneStream,
    from: OutputLinkPortRef,
    to: InputLinkPortRef,
    a_link_connected: ALinkConnected,
    when_the_engine_cannot_wire_it: WhenTheEngineCannotWireALink,
) -> Result<ALinkConnectedOrSetAside> {
    let LiveGraphChangeOnOneStream {
        compiler,
        live,
        stream,
        held_for_one_commit,
        one_input_value_change_at_a_time,
    } = live_graph_change;
    let (from, to) = compiler.scope(|graph, _tx| the_ends_connect_links(graph, from, to))?;

    let applied = {
        let _no_input_value_changing = one_input_value_change_at_a_time.held_while_a_link_is_made();
        apply_a_link(
            &compiler,
            from.clone(),
            to.clone(),
            a_link_connected,
            when_the_engine_cannot_wire_it,
        )?
    };
    let link_id = match applied {
        ALinkConnectedOrSetAside::Connected(link_id) => link_id,
        set_aside @ ALinkConnectedOrSetAside::SetAside(_) => return Ok(set_aside),
    };
    announce_or_hold(
        &stream,
        held_for_one_commit.as_deref(),
        Some(&link_id),
        RuntimeEvent::RuntimeWillConnect {
            from: from.clone(),
            to: to.clone(),
        },
    );

    commit_live_graph_change(&compiler, live).await?;

    announce_or_hold(
        &stream,
        held_for_one_commit.as_deref(),
        Some(&link_id),
        RuntimeEvent::RuntimeDidConnect {
            link_id: link_id.to_string(),
            from,
            to,
        },
    );
    announce_or_hold(
        &stream,
        held_for_one_commit.as_deref(),
        None,
        RuntimeEvent::GraphDidChange,
    );

    Ok(ALinkConnectedOrSetAside::Connected(link_id))
}

/// The ends `from` and `to` as connect links them in `graph`: each port name cast, and a name a
/// port had read as its current one.
fn the_ends_connect_links(
    graph: &Graph,
    from: OutputLinkPortRef,
    to: InputLinkPortRef,
) -> Result<(OutputLinkPortRef, InputLinkPortRef)> {
    let from = from.with_its_port_name_cast()?;
    let to = to.with_its_port_name_cast()?;
    Ok((
        graph.output_under_its_current_name(from),
        graph.input_under_its_current_name(to),
    ))
}

/// Refuse a link from `from` to `to` that connect refuses, in `graph`, before anything is made:
/// an end the graph cannot take, a link its pin types or families refuse, or one whose channel
/// cannot be named. A link of a saved graph, as `a_link_connected` says, keeps one the verdict
/// refuses by its pin types alone when its families take it. Answers the verdict of two typed
/// ends that connect, `None` for ends their families judge, and the channel the link carries
/// on.
fn refuse_a_link_connect_refuses(
    graph: &Graph,
    from: &OutputLinkPortRef,
    to: &InputLinkPortRef,
    a_link_connected: ALinkConnected,
) -> Result<(Option<PinLinkVerdict>, ChannelName)> {
    refuse_a_source_this_graph_cannot_take(graph, from)?;
    refuse_a_destination_this_graph_cannot_take(graph, to)?;
    let verdict =
        refuse_a_link_its_pin_types_or_families_refuse(graph, from, to, a_link_connected)?;
    let channel =
        crate::iceoryx2::source_channel_name(from.node_run_id().as_str(), from.port_name())
            .map_err(|source| Error::InvalidLink(source.to_string()))?;
    Ok((verdict, channel))
}

/// The verdict a connect made now takes on a link from `from` to `to` in the graph `compiler`
/// holds, judged as [`connect_impl`] judges it under [`ALinkConnected::MadeNow`] and
/// [`WhenTheEngineCannotWireALink::Refuse`], and left out of the graph: the verdict of two
/// typed ends that connect, `None` for ends their families judge, refused as connect refuses
/// the link; but a link whose two typed ends connect through a built-in conversion answers that
/// verdict, naming the convert node a transaction's `connect` places between them, once each of
/// the two links through it is one an island takes, carrying the names and codec the end it
/// feeds requires. Connect itself never places it, so a graph written in code stays what its
/// code says.
pub(in crate::core::runtime) fn the_verdict_connect_takes_on_a_link(
    compiler: &Arc<Compiler>,
    from: OutputLinkPortRef,
    to: InputLinkPortRef,
) -> Result<Option<PinLinkVerdict>> {
    compiler.scope(|graph, _tx| the_verdict_connect_takes_in(graph, from, to))
}

/// The verdict a connect made now takes on a link from `from` to `to` in `graph`, as
/// [`the_verdict_connect_takes_on_a_link`] takes it in the graph a compiler holds and
/// [`what_a_connect_makes_of_a_link_at_a_placed_level`] among the nodes of a placed stream's
/// level; the link is left out of `graph`.
fn the_verdict_connect_takes_in(
    graph: &mut Graph,
    from: OutputLinkPortRef,
    to: InputLinkPortRef,
) -> Result<Option<PinLinkVerdict>> {
    let (from, to) = the_ends_connect_links(graph, from, to)?;
    match the_verdict_a_connect_made_now_takes(graph, from.clone(), to.clone()) {
        Ok(verdict) => Ok(verdict),
        Err(refusal @ Error::PinLinkRefused(_)) => {
            let conversions = graph.node_types_this_stream_resolves().pin_conversions();
            let Some(verdict) =
                the_verdict_on_a_link_between_typed_ends(graph, &from, &to, &conversions)
            else {
                return Err(refusal);
            };
            let PinLinkOutcome::ConnectsThroughConvertNode(convert_node) = &verdict.outcome else {
                return Err(refusal);
            };
            refuse_the_links_through_a_node_not_yet_added(
                graph,
                &from,
                &to,
                &convert_node.conversion,
            )?;
            refuse_the_names_and_codec_the_links_through_a_convert_node_refuse(
                graph,
                &from,
                &to,
                convert_node,
            )?;
            Ok(Some(verdict))
        }
        Err(refusal) => Err(refusal),
    }
}

/// The verdict a connect made now takes on the link from `from` to `to` in `graph`: judged with
/// the graph holding the link, as [`connect_impl`] judges it under [`ALinkConnected::MadeNow`]
/// and [`WhenTheEngineCannotWireALink::Refuse`], then taken out again.
fn the_verdict_a_connect_made_now_takes(
    graph: &mut Graph,
    from: OutputLinkPortRef,
    to: InputLinkPortRef,
) -> Result<Option<PinLinkVerdict>> {
    let (verdict, _) = refuse_a_link_connect_refuses(graph, &from, &to, ALinkConnected::MadeNow)?;
    let link_id = add_a_link_unless_the_engine_cannot_wire_it(
        graph,
        from,
        to,
        WhenTheEngineCannotWireALink::Refuse,
    )?
    .connected()?;
    graph.traversal_mut().e(&link_id).drop();
    Ok(verdict)
}

/// Add the link from `from` to `to` to `graph` and judge it as the engine wires it, with the
/// graph holding it, the reader rule as `when_the_engine_cannot_wire_it` says: kept when the
/// engine can wire it, taken out again when it cannot, and then refused or answered set aside.
fn add_a_link_unless_the_engine_cannot_wire_it(
    graph: &mut Graph,
    from: OutputLinkPortRef,
    to: InputLinkPortRef,
    when_the_engine_cannot_wire_it: WhenTheEngineCannotWireALink,
) -> Result<ALinkConnectedOrSetAside> {
    let link_id = graph
        .traversal_mut()
        .add_e(from, to)
        .first()
        .map(|link| link.id.clone())
        .ok_or_else(|| Error::GraphError("failed to create link after validation".into()))?;
    let link = link_in(graph, &link_id)?;
    let judged = refuse_a_held_link_the_engine_cannot_wire(graph, link).and_then(|()| {
        if when_the_engine_cannot_wire_it.judges_the_reader_rule_now() {
            refuse_a_link_leaving_a_reactive_reader_fed_only_by_islands(graph, link)
        } else {
            Ok(())
        }
    });
    let Err(why_the_engine_cannot_wire_it) = judged else {
        return Ok(ALinkConnectedOrSetAside::Connected(link_id));
    };
    graph.traversal_mut().e(&link_id).drop();
    match when_the_engine_cannot_wire_it {
        WhenTheEngineCannotWireALink::SetAsideLeavingTheReaderRuleToASweep => Ok(
            ALinkConnectedOrSetAside::SetAside(why_the_engine_cannot_wire_it),
        ),
        WhenTheEngineCannotWireALink::Refuse
        | WhenTheEngineCannotWireALink::RefuseLeavingTheReaderRuleToASweep => {
            Err(why_the_engine_cannot_wire_it)
        }
    }
}

/// One end of a link the graph holds a node for, the link's other end being a port of a node
/// type no node of the graph has yet.
#[derive(Debug, Clone)]
pub(in crate::core::runtime) enum ALinkEndTheGraphHolds {
    /// The output the link would carry from.
    Output(OutputLinkPortRef),
    /// The input the link would carry into.
    Input(InputLinkPortRef),
}

impl ALinkEndTheGraphHolds {
    /// This end in `graph`, its port name cast and a name its port had read as its current one;
    /// an end whose node or port the graph does not hold is left as given, for its refusal to
    /// name.
    fn under_its_current_port_name(self, graph: &Graph) -> Result<Self> {
        Ok(match self {
            Self::Output(from) => {
                Self::Output(graph.output_under_its_current_name(from.with_its_port_name_cast()?))
            }
            Self::Input(to) => {
                Self::Input(graph.input_under_its_current_name(to.with_its_port_name_cast()?))
            }
        })
    }

    /// The direction of this end's port.
    fn direction(&self) -> PortDirection {
        match self {
            Self::Output(_) => PortDirection::Output,
            Self::Input(_) => PortDirection::Input,
        }
    }

    /// The node and the port name of this end.
    fn node_and_port_name(&self) -> (&NodeRunId, &str) {
        match self {
            Self::Output(from) => (from.node_run_id(), from.port_name()),
            Self::Input(to) => (to.node_run_id(), to.port_name()),
        }
    }

    /// The pin type this end's port carries in `graph`, resolved; `None` for a port of no
    /// resolved pin, or one the graph does not hold.
    fn its_pin_type(&self, graph: &Graph) -> Option<PinType> {
        let (node_run_id, port_name) = self.node_and_port_name();
        graph
            .traversal()
            .v(node_run_id)
            .first()?
            .ports
            .the_one_a_name_finds(self.direction(), port_name)?
            .pin
            .as_ref()
            .map(|pin| pin.pin_type.clone())
    }

    /// The link between this end and port `port_name` of the node `node_run_id`, output first.
    fn the_link_to(
        &self,
        node_run_id: &NodeRunId,
        port_name: &str,
    ) -> (OutputLinkPortRef, InputLinkPortRef) {
        match self {
            Self::Output(from) => (
                from.clone(),
                InputLinkPortRef::new(node_run_id.clone(), port_name),
            ),
            Self::Input(to) => (
                OutputLinkPortRef::new(node_run_id.clone(), port_name),
                to.clone(),
            ),
        }
    }
}

/// Refuse `link`, which `graph` holds, when the engine cannot wire it as the stream plays: one
/// feeding a function node island into a node in the app process, one an island cannot take, or
/// one a channel cannot carry. The rule a reactive reader sets is judged apart, with the link or
/// by a sweep once every link is made.
pub(in crate::core::runtime) fn refuse_a_held_link_the_engine_cannot_wire(
    graph: &Graph,
    link: &Link,
) -> Result<()> {
    refuse_a_link_feeding_an_island_into_the_app_process(graph, link.source(), link.to_port())?;
    refuse_a_link_an_island_cannot_take(graph, link.source(), link.to_port(), Some(&link.id))?;
    refuse_a_link_a_channel_cannot_carry(graph, link)
}

/// A port of a node type the stream has described, as the other end of a link to judge: the spec
/// an add with no input values places a node of the type by, and what the type's ports of the
/// port's direction declare, both read before the graph's lock is taken; the port's declaration
/// is found among them under the current name the placed node gives the port.
pub(in crate::core::runtime) struct ANodeTypesPortToJudge {
    node_type: NodeClassImportPath,
    spec: NodeSpec,
    port_name: String,
    ports_of_its_direction: Arc<[PortDescriptor]>,
}

impl ANodeTypesPortToJudge {
    /// Port `port_name` of `node_type`, as `node_types` resolves the type, among the type's
    /// `ports_of_its_direction`, which every port of the type judged in one batch shares.
    pub(in crate::core::runtime) fn of(
        node_types: &NodeTypesOneStreamResolves,
        node_type: NodeClassImportPath,
        ports_of_its_direction: Arc<[PortDescriptor]>,
        port_name: &str,
    ) -> Self {
        Self {
            spec: node_spec_holding_the_input_values(
                node_types,
                node_type.clone(),
                &serde_json::Map::new(),
                serde_json::Map::new(),
            ),
            node_type,
            port_name: port_name.to_owned(),
            ports_of_its_direction,
        }
    }
}

/// The verdict a connect made now takes on each link between `held`, an end the graph
/// `compiler` holds, and one of `node_types_ports`, in their order, judged as
/// [`the_verdict_connect_takes_on_a_link`] judges a link once an add with no input values had
/// placed a node of the type. The graph's lock is taken once; each node is held in the graph
/// for its one judgement, logging nothing a commit reads, and dropped before the next is placed.
///
/// A link is first judged with the node as placed, as `connect` would judge it after an
/// `add_node`, so a family that refuses the link refuses it here. The port is then judged by
/// what it declares: one of several types as each in turn, answering the one that connects
/// best; any type, or a type found from a link or a setting, as the held end's type, the last
/// warning that it is found only once a node is placed. A port the type does not declare is
/// refused as not found on the type.
pub(in crate::core::runtime) fn the_verdicts_connect_takes_on_links_with_node_types_ports(
    compiler: &Compiler,
    held: ALinkEndTheGraphHolds,
    node_types_ports: Vec<ANodeTypesPortToJudge>,
) -> Result<Vec<Result<Option<PinLinkVerdict>>>> {
    compiler.scope(|graph, _tx| {
        let graph_lock_taken_at = std::time::Instant::now();
        let ports_judged = node_types_ports.len();
        let held = held.under_its_current_port_name(graph)?;
        let the_held_ends_type = held.its_pin_type(graph);
        let mut verdicts = Vec::with_capacity(node_types_ports.len());
        for port in node_types_ports {
            verdicts.push(the_verdict_on_a_link_with_a_node_types_port(
                graph,
                &held,
                the_held_ends_type.as_ref(),
                port,
            ));
        }
        tracing::debug!(
            ports_judged,
            graph_lock_held_for = ?graph_lock_taken_at.elapsed(),
            "node types' ports judged under one hold of the graph's lock"
        );
        Ok(verdicts)
    })
}

/// The verdict a connect made now takes on the link between `held` and `port`, in `graph`, as
/// [`the_verdicts_connect_takes_on_links_with_node_types_ports`] judges each.
fn the_verdict_on_a_link_with_a_node_types_port(
    graph: &mut Graph,
    held: &ALinkEndTheGraphHolds,
    the_held_ends_type: Option<&PinType>,
    ANodeTypesPortToJudge {
        node_type,
        spec,
        port_name,
        ports_of_its_direction,
    }: ANodeTypesPortToJudge,
) -> Result<Option<PinLinkVerdict>> {
    let direction = held.direction().opposite();
    let mut placed = NodeHeldForOneJudgement::placed_in(graph, spec)?;
    let port_name = placed
        .the_name_of_its_port(&port_name, direction)?
        .ok_or_else(|| Error::NodeTypePortNotFound {
            node_type,
            port_name,
            direction,
        })?;
    let (from, to) = held.the_link_to(&placed.node_run_id, &port_name);
    let judge =
        |graph: &mut Graph| the_verdict_a_connect_made_now_takes(graph, from.clone(), to.clone());
    let as_placed = judge(placed.graph_mut())?;
    let Some(declaration) = ports_of_its_direction
        .iter()
        .find(|port| port.name == port_name)
        .and_then(|port| port.pin_declaration.clone())
    else {
        return Ok(as_placed);
    };
    let found_only_once_placed = matches!(
        declaration.pin_type,
        DeclaredPinType::SameAsInput { .. } | DeclaredPinType::SetByInput { .. }
    );
    let types_judged = match declaration.pin_type {
        DeclaredPinType::Is { .. } => return Ok(as_placed),
        DeclaredPinType::OneOf { pin_types } => pin_types,
        DeclaredPinType::Any
        | DeclaredPinType::SameAsInput { .. }
        | DeclaredPinType::SetByInput { .. } => the_held_ends_type.cloned().into_iter().collect(),
    };
    let mut best = None;
    for pin_type in types_judged {
        placed.type_the_port(
            &port_name,
            direction,
            ResolvedPin {
                pin_type,
                flow: declaration.flow,
            },
        );
        let judged = judge(placed.graph_mut());
        if best.as_ref().is_none_or(|best| {
            how_well_a_judged_link_connects(&judged) < how_well_a_judged_link_connects(best)
        }) {
            best = Some(judged);
        }
    }
    let judged = best.unwrap_or(Ok(as_placed))?;
    if !found_only_once_placed {
        return Ok(judged);
    }
    let mut verdict = judged.unwrap_or(PinLinkVerdict {
        outcome: PinLinkOutcome::Connects,
        warnings: Vec::new(),
    });
    verdict
        .warnings
        .push(PinLinkWarning::TypeFoundOnlyOncePlaced);
    Ok(Some(verdict))
}

/// How well a link judged as connect judges it connects, the best first: as it is, through a
/// free conversion, through a convert node, and not at all.
fn how_well_a_judged_link_connects(judged: &Result<Option<PinLinkVerdict>>) -> u8 {
    match judged {
        Ok(None) => 0,
        Ok(Some(verdict)) => match verdict.outcome {
            PinLinkOutcome::Connects => 0,
            PinLinkOutcome::ConnectsWithFreeConversion(_) => 1,
            PinLinkOutcome::ConnectsThroughConvertNode(_) => 2,
            PinLinkOutcome::Refused(_) => 3,
        },
        Err(_) => 4,
    }
}

/// A node of one type placed in a graph, as an add with no input values places it, for one
/// judgement alone, and dropped from it when this is.
struct NodeHeldForOneJudgement<'graph> {
    graph: &'graph mut Graph,
    node_run_id: NodeRunId,
}

impl<'graph> NodeHeldForOneJudgement<'graph> {
    /// A node placed in `graph` by `spec`, refused as the add refuses it.
    fn placed_in(graph: &'graph mut Graph, spec: NodeSpec) -> Result<Self> {
        let node_type = spec.name.clone();
        let node_run_id = graph
            .traversal_mut()
            .add_v(spec)?
            .first()
            .map(|node| node.id.clone())
            .ok_or_else(|| Error::GraphError(format!("a node of `{node_type}` was not placed")))?;
        Ok(Self { graph, node_run_id })
    }

    /// The graph, holding the node, to judge a link to it in.
    fn graph_mut(&mut self) -> &mut Graph {
        self.graph
    }

    /// The current name of the node's port of `direction` that `port_name`, cast, finds;
    /// `None` when it finds none.
    fn the_name_of_its_port(
        &self,
        port_name: &str,
        direction: PortDirection,
    ) -> Result<Option<String>> {
        let port_name = cast_exposed_name_to_url_safe(port_name)?;
        Ok(self
            .graph
            .traversal()
            .v(&self.node_run_id)
            .first()
            .and_then(|node| node.ports.current_name(direction, &port_name))
            .map(str::to_owned))
    }

    /// Type the node's port `port_name` of `direction` as `pin`.
    fn type_the_port(&mut self, port_name: &str, direction: PortDirection, pin: ResolvedPin) {
        let Some(node) = self.graph.traversal_mut().v(&self.node_run_id).first_mut() else {
            return;
        };
        if let Some(port) = node
            .ports
            .on_mut(direction)
            .iter_mut()
            .find(|port| port.name == port_name)
        {
            port.pin = Some(pin);
        }
    }
}

impl Drop for NodeHeldForOneJudgement<'_> {
    fn drop(&mut self) {
        let dropped = self
            .graph
            .drop_the_node(&self.node_run_id, WhyTheNodeLeaves::Removed);
        if !dropped {
            tracing::error!(
                node_run_id = %self.node_run_id,
                "the node held for one judgement was not dropped from the graph"
            );
        }
        debug_assert!(dropped, "the node held for one judgement is dropped");
    }
}

/// What a connect made now would make of `link` at `level`, a level of a placed stream as its
/// graph document holds it, each node by its UUID there, made nowhere: the verdict
/// [`the_verdict_connect_takes_in`] takes among the level's nodes, built from the node types
/// `node_types` resolves, as a load builds them, with the links it holds. Only the level is
/// built, so a check reaching across the placed stream's ports does not judge the link. A link
/// reaching a node that runs as no node of its own, or one that is not built, such as one whose
/// type is not described, is judged only as it is made.
pub(in crate::core::runtime) fn what_a_connect_makes_of_a_link_at_a_placed_level(
    node_types: &Arc<NodeTypesOneStreamResolves>,
    level: &StreamGraphDocument,
    link: &StreamGraphDocumentLink,
) -> Result<WhatALiveConnectMakesOfALink> {
    let (mut graph, node_of_each_node) = the_graph_a_placed_level_runs_as(node_types, level);
    let end = |node: uuid::Uuid| match node_of_each_node.get(&node) {
        Some(NodeOfAPlacedLevel::Built(node_run_id)) => Ok(node_run_id.clone()),
        Some(NodeOfAPlacedLevel::NotBuilt { node_name, why }) => {
            Err(WhatALiveConnectMakesOfALink::ReachesANodeNotBuilt {
                node_name: node_name.clone(),
                why: why.clone(),
            })
        }
        Some(NodeOfAPlacedLevel::RunsAsNoNodeOfItsOwn) | None => {
            Err(WhatALiveConnectMakesOfALink::JudgesItOnlyAsItIsMade)
        }
    };
    let (source, target) = match (end(link.source.node), end(link.target.node)) {
        (Ok(source), Ok(target)) => (source, target),
        (Err(unjudged), _) | (_, Err(unjudged)) => return Ok(unjudged),
    };
    the_verdict_connect_takes_in(
        &mut graph,
        OutputLinkPortRef::new(source, &link.source.port),
        InputLinkPortRef::new(target, &link.target.port),
    )
    .map(|verdict| WhatALiveConnectMakesOfALink::Makes(verdict.map(Box::new)))
}

/// What one node of a placed stream's level is in the graph that level runs as.
enum NodeOfAPlacedLevel {
    /// It is built, under this id.
    Built(NodeRunId),
    /// It runs as no node of its own, such as a fragment.
    RunsAsNoNodeOfItsOwn,
    /// It is not built, for `why`.
    NotBuilt { node_name: String, why: String },
}

/// The graph `level`, a level of a placed stream as its graph document holds it, runs as, made
/// nowhere, and what each of its nodes is in it, by its UUID: each node that runs as a node of
/// its own and whose type `node_types` describes, built, and each link of the level between two
/// built nodes, as a saved graph holds it.
fn the_graph_a_placed_level_runs_as(
    node_types: &Arc<NodeTypesOneStreamResolves>,
    level: &StreamGraphDocument,
) -> (Graph, HashMap<uuid::Uuid, NodeOfAPlacedLevel>) {
    let mut graph = Graph::new_resolving_node_types_through(Arc::clone(node_types));
    let mut node_of_each_node = HashMap::new();
    for node in level.nodes() {
        let node_uuid = node.id;
        let built = if runs_as_no_node_of_its_own(&node) {
            NodeOfAPlacedLevel::RunsAsNoNodeOfItsOwn
        } else {
            match the_node_a_placed_level_builds(&mut graph, node_types, node) {
                Ok(node_run_id) => NodeOfAPlacedLevel::Built(node_run_id),
                Err((node_name, why)) => NodeOfAPlacedLevel::NotBuilt { node_name, why },
            }
        };
        node_of_each_node.insert(node_uuid, built);
    }
    for held in level.links() {
        let built = |node: uuid::Uuid| match node_of_each_node.get(&node) {
            Some(NodeOfAPlacedLevel::Built(node_run_id)) => Some(node_run_id.clone()),
            _ => None,
        };
        let (Some(source), Some(target)) = (built(held.source.node), built(held.target.node))
        else {
            continue;
        };
        if let Ok((from, to)) = the_ends_connect_links(
            &graph,
            OutputLinkPortRef::new(source, held.source.port),
            InputLinkPortRef::new(target, held.target.port),
        ) {
            graph.traversal_mut().add_e(from, to);
        }
    }
    (graph, node_of_each_node)
}

/// The id `node`, a node of a placed stream's level, is built under in `graph`; refused, with
/// its name and why, when its type is not described or a load would not add it.
fn the_node_a_placed_level_builds(
    graph: &mut Graph,
    node_types: &NodeTypesOneStreamResolves,
    node: StreamGraphDocumentNode,
) -> std::result::Result<NodeRunId, (String, String)> {
    let not_built = |why: String| (node.name.clone(), why);
    let node_type = NodeClassImportPath::new(&node.node_type)
        .map_err(|refusal| not_built(format!("its type `{}`: {refusal}", node.node_type)))?;
    if node_types.port_info(&node_type).is_none() {
        return Err(not_built(format!(
            "its type `{}` is not described",
            node.node_type
        )));
    }
    let node_name = node.name.clone();
    graph
        .traversal_mut()
        .add_v(node_spec_of_a_document_node(node_types, node_type, node))
        .and_then(|added| {
            added
                .first()
                .map(|built| built.id.clone())
                .ok_or_else(|| Error::GraphError("the node was not created".into()))
        })
        .map_err(|refusal| (node_name, refusal.to_string()))
}

/// Add the link from `from` to `to` to the graph and queue it for wiring, or, when the engine
/// cannot wire it, refuse it or set it aside as `when_the_engine_cannot_wire_it` says.
fn apply_a_link(
    compiler: &Arc<Compiler>,
    from: OutputLinkPortRef,
    to: InputLinkPortRef,
    a_link_connected: ALinkConnected,
    when_the_engine_cannot_wire_it: WhenTheEngineCannotWireALink,
) -> Result<ALinkConnectedOrSetAside> {
    compiler.scope(|graph, tx| {
        let (_, channel) = refuse_a_link_connect_refuses(graph, &from, &to, a_link_connected)?;
        let made = add_a_link_unless_the_engine_cannot_wire_it(
            graph,
            from,
            to,
            when_the_engine_cannot_wire_it,
        )?;
        if let ALinkConnectedOrSetAside::Connected(link_id) = &made {
            tx.log(PendingOperation::AddLink(link_id.clone()));
            tracing::debug!(
                link_id = %link_id,
                channel = channel.as_str(),
                "connect assigned channel"
            );
        }
        Ok(made)
    })
}
