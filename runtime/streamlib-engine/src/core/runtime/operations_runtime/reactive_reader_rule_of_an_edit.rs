// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The rule a reader that runs reactively sets for an edit of its stream: that something other
//! than a function node island feeds it, judged at each removal and connect made on its own, and
//! once for a batch of graph operations as it closes.

use super::WhenTheEngineCannotWireALink;
use crate::core::Result;
use crate::core::compiler::compiler_ops::function_node_island::{
    runs_inside_the_read_of_each_node_it_feeds,
    the_island_links_into_a_reactive_reader_only_islands_feed, the_reader_a_removal_strands,
    the_refusal_of_a_removal_stranding,
};
use crate::core::graph::{Graph, LinkUniqueId, NodeRunId};
use crate::core::runtime::LoadedStreamInThisRuntime;

/// Refuse `removal` from `graph` when it leaves a reader it does not take out that runs
/// reactively, fed only by function node islands, which have no source to wake it.
pub(super) fn refuse_a_removal_that_strands_a_reactive_reader(
    graph: &Graph,
    removal: ARemoval<'_>,
) -> Result<()> {
    let links_removed: Vec<LinkUniqueId> = graph
        .traversal()
        .e(())
        .iter()
        .filter(|link| match removal {
            ARemoval::OfALink(link_id) => &link.id == link_id,
            ARemoval::OfANode(node_run_id) => {
                link.source().node_run_id() == node_run_id
                    || link.to_port().node_run_id() == node_run_id
            }
        })
        .map(|link| link.id.clone())
        .collect();
    let Some(reader_id) = the_reader_a_removal_strands(graph, &links_removed, |reader_id| {
        !matches!(removal, ARemoval::OfANode(node_run_id) if node_run_id == reader_id)
            && !runs_inside_the_read_of_each_node_it_feeds(graph, reader_id)
    })?
    else {
        return Ok(());
    };
    let removal_named = match removal {
        ARemoval::OfANode(node_run_id) => format!("node `{node_run_id}`"),
        ARemoval::OfALink(link_id) => format!("link `{link_id}`"),
    };
    Err(the_refusal_of_a_removal_stranding(
        &removal_named,
        &reader_id,
    ))
}

/// One edit that takes something out of a stream's graph.
#[derive(Clone, Copy)]
pub(super) enum ARemoval<'removed> {
    OfANode(&'removed NodeRunId),
    OfALink(&'removed LinkUniqueId),
}

impl LoadedStreamInThisRuntime {
    /// Refuse `removal` when it leaves a reader it does not take out that runs reactively, fed
    /// only by function node islands, which have no source to wake it; while a batch of graph
    /// operations is open, that is judged as the batch closes.
    pub(super) fn refuse_a_removal_this_stream_cannot_take(
        &self,
        removal: ARemoval<'_>,
    ) -> Result<()> {
        if self.a_batch_of_graph_operations_is_open() {
            return Ok(());
        }
        self.compiler
            .scope_reading(|graph| refuse_a_removal_that_strands_a_reactive_reader(graph, removal))
    }

    /// Whether a batch of graph operations is open on this stream.
    pub(in crate::core::runtime) fn a_batch_of_graph_operations_is_open(&self) -> bool {
        self.batches_of_graph_operations_open
            .load(std::sync::atomic::Ordering::SeqCst)
            > 0
    }

    /// What a connect an edit makes does with a link the engine cannot wire: refused whole now,
    /// or, while a batch of graph operations is open, the reader rule judged as it closes.
    pub(super) fn the_connect_an_edit_makes(&self) -> WhenTheEngineCannotWireALink {
        if self.a_batch_of_graph_operations_is_open() {
            WhenTheEngineCannotWireALink::RefuseLeavingTheReaderRuleToASweep
        } else {
            WhenTheEngineCannotWireALink::Refuse
        }
    }

    /// Refuse this stream's graph when it holds a link from a function node into a reader that
    /// runs reactively and that only islands feed, which no source would ever wake.
    pub(in crate::core::runtime) fn refuse_a_reactive_reader_only_islands_feed(
        &self,
    ) -> Result<()> {
        self.compiler.scope_reading(|graph| {
            match the_island_links_into_a_reactive_reader_only_islands_feed(graph)
                .into_iter()
                .next()
            {
                Some((_, why_the_engine_cannot_wire_it)) => Err(why_the_engine_cannot_wire_it),
                None => Ok(()),
            }
        })
    }
}
