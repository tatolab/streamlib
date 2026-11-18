// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A link connected between an output port and an input port of one stream's graph, judged
//! before any channel exists, and the same judgement of a link made nowhere.

use std::sync::Arc;

use super::link_ends_this_graph_cannot_take::{
    refuse_a_destination_this_graph_cannot_take, refuse_a_source_this_graph_cannot_take,
};
use super::{LiveGraphChangeOnOneStream, commit_live_graph_change};
use crate::core::compiler::compiler_ops::function_node_island::{
    refuse_a_link_an_island_cannot_take, refuse_a_link_feeding_an_island_into_the_app_process,
    refuse_a_link_leaving_a_reactive_reader_fed_only_by_islands,
};
use crate::core::compiler::compiler_ops::refuse_a_link_a_channel_cannot_carry;
use crate::core::compiler::{Compiler, PendingOperation};
use crate::core::descriptors::PinLinkVerdict;
use crate::core::graph::{
    ALinkConnected, Graph, LinkUniqueId, refuse_a_link_its_pin_types_or_families_refuse,
};
use crate::core::pubsub::RuntimeEvent;
use crate::core::{Error, InputLinkPortRef, OutputLinkPortRef, Result};
use crate::iceoryx2::ChannelName;

/// What connect does with a link the engine cannot wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::core::runtime) enum WhenTheEngineCannotWireALink {
    /// Refuse it, as an edit made now is refused.
    Refuse,
    /// Make nothing and answer why, so a saved graph opens and plays without the link.
    SetAside,
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
    } = live_graph_change;
    let (from, to) = the_ends_connect_links(&compiler, from, to)?;

    stream.publish_on_this_streams_topic(RuntimeEvent::RuntimeWillConnect {
        from: from.clone(),
        to: to.clone(),
    });

    let link_id = match apply_a_link(
        &compiler,
        from.clone(),
        to.clone(),
        a_link_connected,
        when_the_engine_cannot_wire_it,
    )? {
        ALinkConnectedOrSetAside::Connected(link_id) => link_id,
        set_aside @ ALinkConnectedOrSetAside::SetAside(_) => return Ok(set_aside),
    };

    commit_live_graph_change(&compiler, live).await?;

    stream.publish_on_this_streams_topic(RuntimeEvent::RuntimeDidConnect {
        link_id: link_id.to_string(),
        from,
        to,
    });
    stream.publish_on_this_streams_topic(RuntimeEvent::GraphDidChange);

    Ok(ALinkConnectedOrSetAside::Connected(link_id))
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
/// the link.
pub(in crate::core::runtime) fn the_verdict_connect_takes_on_a_link(
    compiler: &Arc<Compiler>,
    from: OutputLinkPortRef,
    to: InputLinkPortRef,
) -> Result<Option<PinLinkVerdict>> {
    let (from, to) = the_ends_connect_links(compiler, from, to)?;
    compiler.scope(|graph, _tx| {
        let (verdict, _) =
            refuse_a_link_connect_refuses(graph, &from, &to, ALinkConnected::MadeNow)?;
        let link_id = add_a_link_unless_the_engine_cannot_wire_it(graph, from, to)??;
        graph.traversal_mut().e(&link_id).drop();
        Ok(verdict)
    })
}

/// Add the link from `from` to `to` to `graph` and judge it as the engine wires it, with the
/// graph holding it: kept when the engine can wire it, taken out again, with why, when it
/// cannot.
fn add_a_link_unless_the_engine_cannot_wire_it(
    graph: &mut Graph,
    from: OutputLinkPortRef,
    to: InputLinkPortRef,
) -> Result<std::result::Result<LinkUniqueId, Error>> {
    let link_id = graph
        .traversal_mut()
        .add_e(from, to)
        .first()
        .map(|link| link.id.clone())
        .ok_or_else(|| Error::GraphError("failed to create link after validation".into()))?;
    match refuse_a_held_link_the_engine_cannot_wire(graph, &link_id) {
        Ok(()) => Ok(Ok(link_id)),
        Err(why_the_engine_cannot_wire_it) => {
            graph.traversal_mut().e(&link_id).drop();
            Ok(Err(why_the_engine_cannot_wire_it))
        }
    }
}

/// Refuse `link_id`, which `graph` holds, when the engine cannot wire it as the stream plays:
/// one feeding a function node island into a node in the app process, one an island cannot
/// take, one leaving a reactive reader fed only by islands, or one a channel cannot carry.
pub(in crate::core::runtime) fn refuse_a_held_link_the_engine_cannot_wire(
    graph: &Graph,
    link_id: &LinkUniqueId,
) -> Result<()> {
    let link = graph
        .traversal()
        .e(link_id)
        .first()
        .ok_or_else(|| Error::LinkNotFound(link_id.to_string()))?;
    refuse_a_link_feeding_an_island_into_the_app_process(graph, link.source(), link.to_port())?;
    refuse_a_link_an_island_cannot_take(graph, link.source(), link.to_port(), Some(link_id))?;
    refuse_a_link_leaving_a_reactive_reader_fed_only_by_islands(graph, link_id)?;
    refuse_a_link_a_channel_cannot_carry(graph, link_id)
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
        match add_a_link_unless_the_engine_cannot_wire_it(graph, from, to)? {
            Ok(link_id) => {
                tx.log(PendingOperation::AddLink(link_id.clone()));
                tracing::debug!(
                    link_id = %link_id,
                    channel = channel.as_str(),
                    "connect assigned channel"
                );
                Ok(ALinkConnectedOrSetAside::Connected(link_id))
            }
            Err(why_the_engine_cannot_wire_it) => match when_the_engine_cannot_wire_it {
                WhenTheEngineCannotWireALink::Refuse => Err(why_the_engine_cannot_wire_it),
                WhenTheEngineCannotWireALink::SetAside => Ok(ALinkConnectedOrSetAside::SetAside(
                    why_the_engine_cannot_wire_it,
                )),
            },
        }
    })
}
