// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A link connected between an output port and an input port of one stream's graph, judged
//! before any channel exists.

use std::sync::Arc;

use super::link_ends_this_graph_cannot_take::{
    refuse_a_destination_this_graph_cannot_take, refuse_a_source_this_graph_cannot_take,
};
use super::{LiveGraphChangeOnOneStream, commit_live_graph_change};
use crate::core::compiler::{Compiler, PendingOperation};
use crate::core::graph::{
    ALinkThePinTypesRefuse, LinkUniqueId, refuse_a_link_its_pin_types_or_families_refuse,
};
use crate::core::pubsub::RuntimeEvent;
use crate::core::{Error, InputLinkPortRef, OutputLinkPortRef, Result};
use crate::iceoryx2::ChannelName;

/// Core implementation for connect - takes owned Arcs for 'static lifetime.
///
/// Connect judges two typed ports by their pin link verdict, and any other two
/// by their families, names, codec and formats, and refuses a link that does
/// not connect as it is by name before any channel exists; one the verdict
/// alone refuses is kept as `a_link_the_pin_types_refuse` says.
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
    let (from, to) = {
        let from = from.with_its_port_name_cast()?;
        let to = to.with_its_port_name_cast()?;
        compiler.scope(|graph, _tx| graph.link_ends_under_their_current_port_names(from, to))
    };

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

/// Add the link from `from` to `to` to the graph and queue it for wiring.
fn apply_a_link(
    compiler: &Arc<Compiler>,
    from: OutputLinkPortRef,
    to: InputLinkPortRef,
    a_link_the_pin_types_refuse: ALinkThePinTypesRefuse,
) -> Result<LinkUniqueId> {
    let (link_id, channel) =
        compiler.scope(|graph, tx| -> Result<(LinkUniqueId, ChannelName)> {
            refuse_a_source_this_graph_cannot_take(graph, &from)?;
            refuse_a_destination_this_graph_cannot_take(graph, &to)?;
            let conversions = graph.node_types_this_stream_resolves().pin_conversions();
            refuse_a_link_its_pin_types_or_families_refuse(
                graph,
                &from,
                &to,
                &conversions,
                a_link_the_pin_types_refuse,
            )?;

            let channel =
                crate::iceoryx2::source_channel_name(from.node_run_id().as_str(), from.port_name())
                    .map_err(|source| Error::InvalidLink(source.to_string()))?;

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
