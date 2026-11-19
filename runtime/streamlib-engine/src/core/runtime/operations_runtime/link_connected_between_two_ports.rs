// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A link connected between an output port and an input port of one stream's graph, judged
//! before any channel exists, and the same judgement of a link made nowhere, in that graph or
//! among the nodes of a placed stream's level.

use std::collections::HashMap;
use std::sync::Arc;

use streamlib_project_repository::{
    StreamGraphDocument, StreamGraphDocumentLink, StreamGraphDocumentNode,
};

use super::link_ends_this_graph_cannot_take::{
    refuse_a_destination_this_graph_cannot_take, refuse_a_source_this_graph_cannot_take,
};
use super::{LiveGraphChangeOnOneStream, commit_live_graph_change};
use crate::core::compiler::compiler_ops::function_node_island::{
    refuse_a_link_an_island_cannot_take, refuse_a_link_feeding_an_island_into_the_app_process,
};
use crate::core::compiler::{Compiler, PendingOperation};
use crate::core::descriptors::{NodeClassImportPath, PinLinkVerdict};
use crate::core::graph::legacy_config_of_a_node::node_spec_of_a_document_node;
use crate::core::graph::{
    ALinkConnected, Graph, LinkUniqueId, NodeRunId, refuse_a_link_its_pin_types_or_families_refuse,
};
use crate::core::nodes::NodeTypesOneStreamResolves;
use crate::core::pubsub::RuntimeEvent;
use crate::core::runtime::WhatALiveConnectMakesOfALink;
use crate::core::runtime::streams_loaded_in_this_runtime::stream_graph_operation_applied_live::runs_as_no_node_of_its_own;
use crate::core::{Error, InputLinkPortRef, OutputLinkPortRef, Result};
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

/// The ends `from` and `to` as connect links them in the graph `compiler` holds: each port
/// name cast, and a name a port had read as its current one.
fn the_ends_connect_links(
    compiler: &Arc<Compiler>,
    from: OutputLinkPortRef,
    to: InputLinkPortRef,
) -> Result<(OutputLinkPortRef, InputLinkPortRef)> {
    compiler.scope(|graph, _tx| the_ends_connect_links_in(graph, from, to))
}

/// The ends `from` and `to` as connect links them in `graph`: each port name cast, and a name a
/// port had read as its current one.
fn the_ends_connect_links_in(
    graph: &Graph,
    from: OutputLinkPortRef,
    to: InputLinkPortRef,
) -> Result<(OutputLinkPortRef, InputLinkPortRef)> {
    let from = from.with_its_port_name_cast()?;
    let to = to.with_its_port_name_cast()?;
    Ok(graph.link_ends_under_their_current_port_names(from, to))
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
/// nowhere, as [`the_verdict_connect_takes_in`] takes it.
pub(in crate::core::runtime) fn the_verdict_connect_takes_on_a_link(
    compiler: &Arc<Compiler>,
    from: OutputLinkPortRef,
    to: InputLinkPortRef,
) -> Result<Option<PinLinkVerdict>> {
    compiler.scope(|graph, _tx| the_verdict_connect_takes_in(graph, from, to))
}

/// The verdict a connect made now takes on a link from `from` to `to` in `graph`, judged as
/// [`connect_impl`] judges it under [`ALinkConnected::MadeNow`] and made nowhere: the verdict
/// of two typed ends that connect, `None` for ends their families judge, refused as connect
/// refuses the link.
fn the_verdict_connect_takes_in(
    graph: &Graph,
    from: OutputLinkPortRef,
    to: InputLinkPortRef,
) -> Result<Option<PinLinkVerdict>> {
    let (from, to) = the_ends_connect_links_in(graph, from, to)?;
    refuse_a_link_connect_refuses(graph, &from, &to, ALinkConnected::MadeNow)
        .map(|(verdict, _)| verdict)
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
    let (graph, node_of_each_node) = the_graph_a_placed_level_runs_as(node_types, level);
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
        &graph,
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
        if let Ok((from, to)) = the_ends_connect_links_in(
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
