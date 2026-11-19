// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A link disconnected: marked pending deletion, its removal logged for the next commit, and
//! announced, or held with the graph edits held for one commit.

use super::{
    GraphEditsHeldForOneCommit, LiveGraphChangeOnOneStream, announce_or_hold,
    commit_live_graph_change,
};
use crate::core::compiler::{Compiler, PendingOperation};
use crate::core::graph::{GraphEdgeWithComponents, LinkUniqueId, PendingDeletionComponent};
use crate::core::pubsub::{LoadedStreamIdentity, RuntimeEvent};
use crate::core::runtime::LoadedStreamInThisRuntime;
use crate::core::{Error, InputLinkPortRef, OutputLinkPortRef, Result};

/// Core implementation for disconnect - takes owned Arcs for 'static lifetime.
pub(in crate::core::runtime) async fn disconnect_impl(
    live_graph_change: LiveGraphChangeOnOneStream,
    link_id: LinkUniqueId,
) -> Result<()> {
    let LiveGraphChangeOnOneStream {
        compiler,
        live,
        stream,
        held_for_one_commit,
    } = live_graph_change;
    let (from_port, to_port) = take_the_link_out_with_the_next_commit(&compiler, &link_id)?;
    commit_live_graph_change(&compiler, live).await?;
    announce_or_hold_a_disconnect(
        &stream,
        held_for_one_commit.as_deref(),
        &link_id,
        &from_port,
        &to_port,
    );
    Ok(())
}

/// Mark `link_id` pending deletion and log its removal, so the next commit takes it out;
/// answers its ends.
fn take_the_link_out_with_the_next_commit(
    compiler: &Compiler,
    link_id: &LinkUniqueId,
) -> Result<(OutputLinkPortRef, InputLinkPortRef)> {
    compiler.scope(|graph, tx| {
        let ends = graph
            .traversal()
            .e(link_id)
            .first()
            .map(|link| (link.source().clone(), link.to_port().clone()))
            .ok_or_else(|| Error::NotFound(format!("Link '{link_id}' not found")))?;
        if let Some(link) = graph.traversal_mut().e(link_id).first_mut() {
            link.insert(PendingDeletionComponent);
        }
        tx.log(PendingOperation::RemoveLink(link_id.clone()));
        Ok(ends)
    })
}

/// Announce, or hold with the graph edits held for one commit, the disconnect of `link_id`
/// from `from_port` to `to_port`.
fn announce_or_hold_a_disconnect(
    stream: &LoadedStreamIdentity,
    held_for_one_commit: Option<&GraphEditsHeldForOneCommit>,
    link_id: &LinkUniqueId,
    from_port: &OutputLinkPortRef,
    to_port: &InputLinkPortRef,
) {
    for event in [
        RuntimeEvent::RuntimeWillDisconnect {
            link_id: link_id.to_string(),
            from_port: from_port.to_string(),
            to_port: to_port.to_string(),
        },
        RuntimeEvent::RuntimeDidDisconnect {
            link_id: link_id.to_string(),
            from_port: from_port.to_string(),
            to_port: to_port.to_string(),
        },
    ] {
        announce_or_hold(stream, held_for_one_commit, Some(link_id), event);
    }
    announce_or_hold(
        stream,
        held_for_one_commit,
        None,
        RuntimeEvent::GraphDidChange,
    );
}

impl LoadedStreamInThisRuntime {
    /// Disconnect `link_id` within the graph edits the running task holds for one commit,
    /// which take it out as they close; refused outside any.
    pub(in crate::core::runtime) fn disconnect_within_the_graph_edits_held(
        &self,
        link_id: &LinkUniqueId,
    ) -> Result<()> {
        let Some(held_for_one_commit) = GraphEditsHeldForOneCommit::the_ones_this_task_holds()
        else {
            return Err(Error::Runtime(format!(
                "link `{link_id}` is disconnected within graph edits held for one commit, and \
                 none are held"
            )));
        };
        let (from_port, to_port) = take_the_link_out_with_the_next_commit(&self.compiler, link_id)?;
        announce_or_hold_a_disconnect(
            self.loaded_stream_identity(),
            Some(&held_for_one_commit),
            link_id,
            &from_port,
            &to_port,
        );
        Ok(())
    }
}
