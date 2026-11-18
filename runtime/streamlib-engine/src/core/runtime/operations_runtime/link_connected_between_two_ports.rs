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
    DeclaredPinType, NodeClassImportPath, PinDeclaration, PinLinkOutcome, PinLinkVerdict,
    PinLinkWarning, ResolvedPin,
};
use crate::core::graph::legacy_config_of_a_node::config_of_a_node;
use crate::core::graph::{
    ALinkConnected, Graph, LinkUniqueId, NodeRunId, WhyTheNodeLeaves,
    refuse_a_link_its_pin_types_or_families_refuse,
};
use crate::core::nodes::NodeSpec;
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
    let (from, to) = the_ends_connect_links(&compiler, from, to)?;

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

/// The ends `from` and `to` as connect links them: each port name cast, and a name a port had
/// read as its current one.
fn the_ends_connect_links(
    compiler: &Arc<Compiler>,
    from: OutputLinkPortRef,
    to: InputLinkPortRef,
) -> Result<(OutputLinkPortRef, InputLinkPortRef)> {
    let from = from.with_its_port_name_cast()?;
    let to = to.with_its_port_name_cast()?;
    Ok(compiler.scope(|graph, _tx| graph.link_ends_under_their_current_port_names(from, to)))
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
    let (from, to) = the_ends_connect_links(compiler, from, to)?;
    compiler.scope(|graph, _tx| {
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

    /// The direction of the link's other end.
    fn the_other_ends_direction(&self) -> PortDirection {
        match self {
            Self::Output(_) => PortDirection::Input,
            Self::Input(_) => PortDirection::Output,
        }
    }
}

/// The verdict a connect made now takes on a link between `held`, an end the graph `compiler`
/// holds, and port `port_name` of `node_type`, judged as [`the_verdict_connect_takes_on_a_link`]
/// judges a link once an add with no input values had placed a node of the type: that node is
/// held in the graph for the judgement alone, under the graph's lock, logging nothing a commit
/// reads, and dropped before the lock is let go.
///
/// The port is judged by what it declares. One of several types is judged as each in turn, and
/// answers the one that connects best. Any type, or a type found from a link or a setting, is
/// judged as the held end's type, the last warning that it is found only once a node is placed;
/// with the held end untyped it is judged by its family. A node type the stream does not
/// resolve is refused as unknown, and a port it does not declare as not found on the type.
pub(in crate::core::runtime) fn the_verdict_connect_takes_on_a_link_with_a_node_types_port(
    compiler: &Arc<Compiler>,
    held: &ALinkEndTheGraphHolds,
    node_type: &NodeClassImportPath,
    port_name: &str,
) -> Result<Option<PinLinkVerdict>> {
    compiler.scope(|graph, _tx| {
        let mut placed = NodeHeldForOneJudgement::placed_in(graph, node_type)?;
        let (from, to) = held.the_link_to(&placed.node_run_id, port_name);
        let (from, to) = placed.graph().link_ends_under_their_current_port_names(
            from.with_its_port_name_cast()?,
            to.with_its_port_name_cast()?,
        );
        let direction = held.the_other_ends_direction();
        let port_name = match direction {
            PortDirection::Input => to.port_name(),
            PortDirection::Output => from.port_name(),
        }
        .to_owned();
        if !placed.has_the_port(&port_name, direction) {
            return Err(Error::NodePortNotFound {
                node_run_id: node_type.to_string(),
                port_name,
                direction,
            });
        }
        let judge = |graph: &Graph| {
            refuse_a_link_connect_refuses(graph, &from, &to, ALinkConnected::MadeNow)
                .map(|(verdict, _)| verdict)
        };
        let Some(declaration) =
            the_pin_declaration_of(placed.graph(), node_type, &port_name, direction)
        else {
            return judge(placed.graph());
        };
        let the_held_ends_type = || {
            let (node_run_id, held_port_name, held_direction) = match held {
                ALinkEndTheGraphHolds::Output(_) => {
                    (from.node_run_id(), from.port_name(), PortDirection::Output)
                }
                ALinkEndTheGraphHolds::Input(_) => {
                    (to.node_run_id(), to.port_name(), PortDirection::Input)
                }
            };
            the_resolved_pin_of(placed.graph(), node_run_id, held_port_name, held_direction)
                .map(|pin| pin.pin_type)
        };
        let types_judged: Vec<_> = match &declaration.pin_type {
            DeclaredPinType::Is { .. } => return judge(placed.graph()),
            DeclaredPinType::OneOf { pin_types } => pin_types.clone(),
            DeclaredPinType::Any
            | DeclaredPinType::SameAsInput { .. }
            | DeclaredPinType::SetByInput { .. } => the_held_ends_type().into_iter().collect(),
        };
        let found_only_once_placed = matches!(
            declaration.pin_type,
            DeclaredPinType::SameAsInput { .. } | DeclaredPinType::SetByInput { .. }
        );
        let judged = types_judged
            .into_iter()
            .map(|pin_type| {
                placed.type_the_port(
                    &port_name,
                    direction,
                    ResolvedPin {
                        pin_type,
                        flow: declaration.flow,
                    },
                );
                judge(placed.graph())
            })
            .min_by_key(how_well_a_judged_link_connects)
            .unwrap_or_else(|| judge(placed.graph()));
        if !found_only_once_placed {
            return judged;
        }
        judged.map(|verdict| {
            let mut verdict = verdict.unwrap_or(PinLinkVerdict {
                outcome: PinLinkOutcome::Connects,
                warnings: Vec::new(),
            });
            verdict
                .warnings
                .push(PinLinkWarning::TypeFoundOnlyOncePlaced);
            Some(verdict)
        })
    })
}

/// The pin `node_type`'s port `port_name` of `direction` declares, as `graph`'s stream resolves
/// the type; `None` for a port that declares none.
fn the_pin_declaration_of(
    graph: &Graph,
    node_type: &NodeClassImportPath,
    port_name: &str,
    direction: PortDirection,
) -> Option<PinDeclaration> {
    let descriptor = graph
        .node_types_this_stream_resolves()
        .descriptor(node_type)?;
    let ports = match direction {
        PortDirection::Input => descriptor.inputs,
        PortDirection::Output => descriptor.outputs,
    };
    ports
        .into_iter()
        .find(|port| port.name == port_name)?
        .pin_declaration
}

/// The pin port `port_name` of `direction` of the node `node_run_id` carries, resolved; `None`
/// for a port of no resolved pin, or one the node does not have.
fn the_resolved_pin_of(
    graph: &Graph,
    node_run_id: &NodeRunId,
    port_name: &str,
    direction: PortDirection,
) -> Option<ResolvedPin> {
    let node = graph.traversal().v(node_run_id).first()?;
    let port = match direction {
        PortDirection::Input => node.input_port(port_name),
        PortDirection::Output => node.output_port(port_name),
    }?;
    port.pin.clone()
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
    /// A node of `node_type` placed in `graph`; a type the stream does not resolve is refused
    /// as unknown, and one it cannot add as the add refuses it.
    fn placed_in(graph: &'graph mut Graph, node_type: &NodeClassImportPath) -> Result<Self> {
        let node_types = Arc::clone(graph.node_types_this_stream_resolves());
        if node_types.port_info(node_type).is_none() {
            return Err(Error::UnknownNodeType {
                ident: node_type.clone(),
            });
        }
        let config = config_of_a_node(&node_types, node_type, &serde_json::Map::new());
        let node_run_id = graph
            .traversal_mut()
            .add_v(NodeSpec::new(node_type.clone(), config))?
            .first()
            .map(|node| node.id.clone())
            .ok_or_else(|| Error::GraphError(format!("a node of `{node_type}` was not placed")))?;
        Ok(Self { graph, node_run_id })
    }

    /// The graph, holding the node.
    fn graph(&self) -> &Graph {
        self.graph
    }

    /// Whether the node has a port `port_name` of `direction`.
    fn has_the_port(&self, port_name: &str, direction: PortDirection) -> bool {
        self.graph
            .traversal()
            .v(&self.node_run_id)
            .first()
            .is_some_and(|node| match direction {
                PortDirection::Input => node.has_input(port_name),
                PortDirection::Output => node.has_output(port_name),
            })
    }

    /// Type the node's port `port_name` of `direction` as `pin`.
    fn type_the_port(&mut self, port_name: &str, direction: PortDirection, pin: ResolvedPin) {
        let Some(node) = self.graph.traversal_mut().v(&self.node_run_id).first_mut() else {
            return;
        };
        let ports = match direction {
            PortDirection::Input => &mut node.ports.inputs,
            PortDirection::Output => &mut node.ports.outputs,
        };
        if let Some(port) = ports.iter_mut().find(|port| port.name == port_name) {
            port.pin = Some(pin);
        }
    }
}

impl Drop for NodeHeldForOneJudgement<'_> {
    fn drop(&mut self) {
        let dropped = self
            .graph
            .drop_the_node(&self.node_run_id, WhyTheNodeLeaves::Removed);
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
