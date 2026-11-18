// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A link connected between an output port and an input port of one stream's graph, judged
//! before any channel exists, and the same judgement of a link made nowhere.

use std::sync::Arc;

use super::link_ends_this_graph_cannot_take::{
    refuse_a_destination_this_graph_cannot_take, refuse_a_source_this_graph_cannot_take,
};
use super::{LiveGraphChangeOnOneStream, commit_live_graph_change};
use crate::core::compiler::compiler_ops::function_node_island::refuse_a_link_feeding_an_island_into_the_app_process;
use crate::core::compiler::{Compiler, PendingOperation};
use crate::core::descriptors::PinLinkVerdict;
use crate::core::graph::{
    ALinkThePinTypesRefuse, Graph, LinkUniqueId, refuse_a_link_its_pin_types_or_families_refuse,
};
use crate::core::pubsub::RuntimeEvent;
use crate::core::{Error, InputLinkPortRef, OutputLinkPortRef, Result};
use crate::iceoryx2::ChannelName;

/// Core implementation for connect - takes owned Arcs for 'static lifetime.
///
/// Connect judges two typed ports by their pin link verdict, and any other two
/// by their families, names, codec and formats, and refuses a link that does
/// not connect as it is, or that feeds a function node island into a node in
/// the app process, by name before any channel exists; one the verdict alone
/// refuses is kept as `a_link_the_pin_types_refuse` says.
#[tracing::instrument(
    name = "stream.connect",
    skip(live_graph_change),
    fields(from = %from, to = %to),
)]
pub(in crate::core::runtime) async fn connect_impl(
    live_graph_change: LiveGraphChangeOnOneStream,
    from: OutputLinkPortRef,
    to: InputLinkPortRef,
    a_link_the_pin_types_refuse: ALinkThePinTypesRefuse,
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

    let link_id = apply_a_link(
        &compiler,
        from.clone(),
        to.clone(),
        a_link_the_pin_types_refuse,
    )?;

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
/// an end the graph cannot take, a link its pin types or families refuse (one the verdict alone
/// refuses kept as `a_link_the_pin_types_refuse` says), one feeding a function node island into
/// the app process, or one whose channel cannot be named. Answers the verdict of two typed ends
/// that connect, `None` for ends their families judge, and the channel the link carries on.
fn refuse_a_link_connect_refuses(
    graph: &Graph,
    from: &OutputLinkPortRef,
    to: &InputLinkPortRef,
    a_link_the_pin_types_refuse: ALinkThePinTypesRefuse,
) -> Result<(Option<PinLinkVerdict>, ChannelName)> {
    refuse_a_source_this_graph_cannot_take(graph, from)?;
    refuse_a_destination_this_graph_cannot_take(graph, to)?;
    let verdict = refuse_a_link_its_pin_types_or_families_refuse(
        graph,
        from,
        to,
        a_link_the_pin_types_refuse,
    )?;
    refuse_a_link_feeding_an_island_into_the_app_process(graph, from, to)?;
    let channel =
        crate::iceoryx2::source_channel_name(from.node_run_id().as_str(), from.port_name())
            .map_err(|source| Error::InvalidLink(source.to_string()))?;
    Ok((verdict, channel))
}

/// The verdict a connect made now takes on a link from `from` to `to` in the graph `compiler`
/// holds, judged as [`connect_impl`] judges it under [`ALinkThePinTypesRefuse::IsRefused`] and
/// made nowhere: the verdict of two typed ends that connect, `None` for ends their families
/// judge, refused as connect refuses the link.
pub(in crate::core::runtime) fn the_verdict_connect_takes_on_a_link(
    compiler: &Arc<Compiler>,
    from: OutputLinkPortRef,
    to: InputLinkPortRef,
) -> Result<Option<PinLinkVerdict>> {
    let (from, to) = the_ends_connect_links(compiler, from, to)?;
    compiler.scope(|graph, _tx| {
        refuse_a_link_connect_refuses(graph, &from, &to, ALinkThePinTypesRefuse::IsRefused)
            .map(|(verdict, _)| verdict)
    })
}

/// Add the link from `from` to `to` to the graph and queue it for wiring.
fn apply_a_link(
    compiler: &Arc<Compiler>,
    from: OutputLinkPortRef,
    to: InputLinkPortRef,
    a_link_the_pin_types_refuse: ALinkThePinTypesRefuse,
) -> Result<LinkUniqueId> {
    let (link_id, channel) =
        compiler.scope(|graph, tx| -> Result<(LinkUniqueId, ChannelName)> {
            let (_, channel) =
                refuse_a_link_connect_refuses(graph, &from, &to, a_link_the_pin_types_refuse)?;

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
