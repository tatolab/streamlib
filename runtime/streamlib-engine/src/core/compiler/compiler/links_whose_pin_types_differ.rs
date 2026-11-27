// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A compile's part in an input that chooses a pin type: each link its node's type choices reach
//! is judged again by the types its two ends carry now. One whose ends no longer connect is
//! unwired and kept, carrying nothing and in error for it, and one that connects again is wired
//! again; no node is rebuilt for either, and a link held unwired so is never wired by a restart
//! or a start until its ends connect.

use super::{CompilationPlan, this_link_is_already_wired_or_awaiting_its_helpers_answer};
use crate::core::compiler::compiler_ops::function_node_island::{
    HowALinkIsWired, how_a_link_is_wired,
};
use crate::core::compiler::compiler_ops::running_node_restart::unwire_each_link_keeping_it;
use crate::core::error::Result;
use crate::core::graph::{
    Graph, GraphEdgeWithComponents, LinkState, LinkStateComponent, LinkUniqueId,
};

/// Judge again each link the type choices of each node `plan` names reach, and each link
/// marked as differing, changing nothing in `graph`: queue on `plan` each link whose ends differ
/// that is still wired through a channel to be held unwired, and each that connects again to be
/// wired; a link `plan` removes is neither.
pub(super) fn judge_the_links_each_type_choice_reaches(graph: &Graph, plan: &mut CompilationPlan) {
    let mut judged_again = graph.judge_again_each_link_the_type_choices_of_the_nodes_reach(
        &plan.nodes_whose_type_choices_are_judged_again,
    );
    judged_again
        .differing
        .retain(|(link_id, _)| !plan.links_to_remove.contains(link_id));
    judged_again
        .connecting_again
        .retain(|link_id| !plan.links_to_remove.contains(link_id));
    for (link_id, _) in &judged_again.differing {
        let wired = graph
            .traversal()
            .e(link_id)
            .first()
            .is_some_and(this_link_is_already_wired_or_awaiting_its_helpers_answer);
        if wired && is_unwired_by_closing_its_channel(graph, link_id) {
            plan.links_to_hold_unwired.push(link_id.clone());
        } else if wired {
            tracing::warn!(
                "{link_id} no longer connects its two ends, and is a call inside an island, \
                 which keeps running until the stream starts again"
            );
        }
    }
    for link_id in &judged_again.connecting_again {
        if !plan.links_to_add.contains(link_id) {
            plan.links_to_add.push(link_id.clone());
        }
    }
    plan.links_judged_again = judged_again;
}

/// Whether `link_id` is unwired by closing the channel it was wired through, as
/// [`unwire_each_link_keeping_it`] closes it: not a call inside an island, which has none.
fn is_unwired_by_closing_its_channel(graph: &Graph, link_id: &LinkUniqueId) -> bool {
    !matches!(
        how_a_link_is_wired(graph, link_id),
        Ok(HowALinkIsWired::AsACallInsideAFunctionNodeIsland
            | HowALinkIsWired::AsACallInsideAShaderNodeIsland)
            | Err(_)
    )
}

/// Mark on `graph` each link `plan` judged again, then close the channel of each it holds
/// unwired, its ends no longer carrying types that connect, keeping each in the graph in error,
/// so it carries nothing.
pub(super) fn hold_each_link_unwired(graph: &mut Graph, plan: &CompilationPlan) -> Result<()> {
    graph.mark_each_link_judged_again(&plan.links_judged_again);
    let links = &plan.links_to_hold_unwired;
    if links.is_empty() {
        return Ok(());
    }
    unwire_each_link_keeping_it(graph, links)?;
    for link_id in links {
        tracing::info!(
            "Holding {link_id} unwired: its two ends no longer carry types that connect"
        );
        stamp_the_link_in_error(graph, link_id);
    }
    Ok(())
}

/// Stamp `link_id` in error, its ends no longer carrying types that connect, so it reads as
/// held unwired.
pub(super) fn stamp_the_link_in_error(graph: &mut Graph, link_id: &LinkUniqueId) {
    if let Some(link) = graph.traversal_mut().e(link_id).first_mut() {
        link.insert(LinkStateComponent(LinkState::Error));
    }
}
