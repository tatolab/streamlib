// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The rule a reader that runs reactively sets for an edit of its stream: that something other
//! than a function node island feeds it, judged at each removal and connect made on its own, and
//! once as graph edits held for one commit close.

use super::WhenTheEngineCannotWireALink;
use super::graph_edits_in_one_commit::GraphEditsHeldForOneCommit;
use crate::core::Result;
use crate::core::compiler::compiler_ops::function_node_island::{
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
    // The readers a removed link reaches never run inside an island, so only a removed reader
    // is left out of the judgement.
    let Some(reader_id) = the_reader_a_removal_strands(
        graph,
        &links_removed,
        |reader_id| !matches!(removal, ARemoval::OfANode(node_run_id) if node_run_id == reader_id),
    )?
    else {
        return Ok(());
    };
    Err(the_refusal_of_a_removal_stranding(
        &removal.to_string(),
        &reader_id,
    ))
}

/// One edit that takes something out of a stream's graph.
#[derive(Clone, Copy, Debug)]
pub(super) enum ARemoval<'removed> {
    OfANode(&'removed NodeRunId),
    OfALink(&'removed LinkUniqueId),
}

impl std::fmt::Display for ARemoval<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::OfANode(node_run_id) => write!(formatter, "node `{node_run_id}`"),
            Self::OfALink(link_id) => write!(formatter, "link `{link_id}`"),
        }
    }
}

/// Whether the rule a reader that runs reactively sets is left to the close of the graph edits
/// this task holds for one commit, rather than judged at each edit: the one place an edit asks.
pub(in crate::core::runtime) fn the_reader_rule_is_left_to_the_close() -> bool {
    GraphEditsHeldForOneCommit::the_ones_this_task_holds().is_some()
}

impl LoadedStreamInThisRuntime {
    /// Refuse `removal` when it leaves a reader it does not take out that runs reactively, fed
    /// only by function node islands, which have no source to wake it; judged as the graph
    /// edits this task holds close, when it holds some.
    pub(super) fn refuse_a_removal_this_stream_cannot_take(
        &self,
        removal: ARemoval<'_>,
    ) -> Result<()> {
        if the_reader_rule_is_left_to_the_close() {
            return Ok(());
        }
        self.compiler
            .scope_reading(|graph| refuse_a_removal_that_strands_a_reactive_reader(graph, removal))
    }

    /// What a connect an edit makes does with a link the engine cannot wire: refused whole now,
    /// or with the reader rule left to the close of the graph edits this task holds.
    pub(in crate::core::runtime) fn the_connect_an_edit_makes(
        &self,
    ) -> WhenTheEngineCannotWireALink {
        if the_reader_rule_is_left_to_the_close() {
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
            match the_island_links_into_a_reactive_reader_only_islands_feed(graph).next() {
                Some((_, why_the_engine_cannot_wire_it)) => Err(why_the_engine_cannot_wire_it),
                None => Ok(()),
            }
        })
    }
}
