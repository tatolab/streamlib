// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A link connected between an output port and an input port of one stream's graph, judged
//! before any channel exists, and the same judgement of a link made nowhere: between two ends
//! the graph holds, or between one it holds and a port of a node type no node has yet.

use std::sync::Arc;

use super::link_ends_this_graph_cannot_take::{
    refuse_a_destination_this_graph_cannot_take, refuse_a_source_this_graph_cannot_take,
};
use super::{LiveGraphChangeOnOneStream, commit_live_graph_change};
use crate::core::compiler::compiler_ops::function_node_island::{
    refuse_a_link_an_island_cannot_take, refuse_a_link_feeding_an_island_into_the_app_process,
};
use crate::core::compiler::{Compiler, PendingOperation};
use crate::core::descriptors::{
    DeclaredPinType, NodeClassImportPath, PinLinkOutcome, PinLinkVerdict, PinLinkWarning, PinType,
    PortDescriptor, ResolvedPin,
};
use crate::core::graph::legacy_config_of_a_node::node_spec_holding_the_input_values;
use crate::core::graph::{
    ALinkConnected, Graph, LinkUniqueId, NodeRunId, WhyTheNodeLeaves,
    cast_exposed_name_to_url_safe, refuse_a_link_its_pin_types_or_families_refuse,
};
use crate::core::nodes::{NodeSpec, NodeTypesOneStreamResolves};
use crate::core::pubsub::RuntimeEvent;
use crate::core::{Error, InputLinkPortRef, OutputLinkPortRef, PortDirection, Result};
use crate::iceoryx2::ChannelName;

/// Core implementation for connect - takes owned Arcs for 'static lifetime.
///
/// Connect judges two typed ports by their pin link verdict, and any other two by their
/// families, names, codec and formats, and refuses, by name before any channel exists, a link
/// that does not connect as it is, that feeds a function node island into a node in the app
/// process, or that an island cannot take; `a_link_connected` says how leniently a link of a
/// saved graph is judged.
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
) -> Result<LinkUniqueId> {
    let LiveGraphChangeOnOneStream {
        compiler,
        live,
        stream,
    } = live_graph_change;
    let (from, to) = compiler.scope(|graph, _tx| the_ends_connect_links(graph, from, to))?;

    stream.publish_on_this_streams_topic(RuntimeEvent::RuntimeWillConnect {
        from: from.clone(),
        to: to.clone(),
    });

    let link_id = apply_a_link(&compiler, from.clone(), to.clone(), a_link_connected)?;

    commit_live_graph_change(&compiler, live).await?;

    stream.publish_on_this_streams_topic(RuntimeEvent::RuntimeDidConnect {
        link_id: link_id.to_string(),
        from,
        to,
    });
    stream.publish_on_this_streams_topic(RuntimeEvent::GraphDidChange);

    Ok(link_id)
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
/// an end the graph cannot take, a link its pin types or families refuse, one feeding a
/// function node island into the app process, one an island cannot take, or one whose channel
/// cannot be named. A link of a saved graph, as `a_link_connected` says, keeps one the verdict
/// refuses by its pin types alone when its families take it, and leaves one an island cannot
/// take to the commit that builds the stream, so the graph opens. Answers the verdict of two
/// typed ends that connect, `None` for ends their families judge, and the channel the link
/// carries on.
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
    refuse_a_link_feeding_an_island_into_the_app_process(graph, from, to)?;
    if a_link_connected == ALinkConnected::MadeNow {
        refuse_a_link_an_island_cannot_take(graph, from, to, None)?;
    }
    let channel =
        crate::iceoryx2::source_channel_name(from.node_run_id().as_str(), from.port_name())
            .map_err(|source| Error::InvalidLink(source.to_string()))?;
    Ok((verdict, channel))
}

/// The verdict a connect made now takes on a link from `from` to `to` in the graph `compiler`
/// holds, judged as [`connect_impl`] judges it under [`ALinkConnected::MadeNow`] and made
/// nowhere: the verdict of two typed ends that connect, `None` for ends their families
/// judge, refused as connect refuses the link.
pub(in crate::core::runtime) fn the_verdict_connect_takes_on_a_link(
    compiler: &Arc<Compiler>,
    from: OutputLinkPortRef,
    to: InputLinkPortRef,
) -> Result<Option<PinLinkVerdict>> {
    compiler.scope(|graph, _tx| {
        let (from, to) = the_ends_connect_links(graph, from, to)?;
        refuse_a_link_connect_refuses(graph, &from, &to, ALinkConnected::MadeNow)
            .map(|(verdict, _)| verdict)
    })
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
    let judge = |graph: &Graph| {
        refuse_a_link_connect_refuses(graph, &from, &to, ALinkConnected::MadeNow)
            .map(|(verdict, _)| verdict)
    };
    let as_placed = judge(placed.graph())?;
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
        let judged = judge(placed.graph());
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

    /// The graph, holding the node.
    fn graph(&self) -> &Graph {
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

/// Add the link from `from` to `to` to the graph and queue it for wiring.
fn apply_a_link(
    compiler: &Arc<Compiler>,
    from: OutputLinkPortRef,
    to: InputLinkPortRef,
    a_link_connected: ALinkConnected,
) -> Result<LinkUniqueId> {
    let (link_id, channel) =
        compiler.scope(|graph, tx| -> Result<(LinkUniqueId, ChannelName)> {
            let (_, channel) = refuse_a_link_connect_refuses(graph, &from, &to, a_link_connected)?;

            let link_id = graph
                .traversal_mut()
                .add_e(from, to)
                .inspect(|link| tx.log(PendingOperation::AddLink(link.id.clone())))
                .first()
                .map(|link| link.id.clone())
                .ok_or_else(|| {
                    Error::GraphError("failed to create link after validation".into())
                })?;

            Ok((link_id, channel))
        })?;

    tracing::debug!(
        link_id = %link_id,
        channel = channel.as_str(),
        "connect assigned channel"
    );
    Ok(link_id)
}
