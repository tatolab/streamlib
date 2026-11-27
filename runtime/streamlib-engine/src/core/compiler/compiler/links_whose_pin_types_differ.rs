// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A compile's part in an input that chooses a pin type: each link its node's type choices reach
//! is judged again by the types its two ends carry now. One whose ends no longer connect is
//! unwired and kept, carrying nothing and in error for it, and one that connects again is wired
//! again; no node is rebuilt for either, and a link held unwired so is never wired by a restart
//! or a start until its ends connect.

use super::{CompilationPlan, this_link_is_already_wired_or_awaiting_its_helpers_answer};
use crate::core::compiler::compiler_ops::running_node_restart::unwire_each_link_keeping_it;
use crate::core::error::Result;
use crate::core::graph::{
    Graph, GraphEdgeWithComponents, LinkPinTypesDifferComponent, LinkState, LinkStateComponent,
    LinkUniqueId,
};

/// Judge again each link the type choices of each node `plan` names reach, and queue on `plan`
/// each link whose ends differ that is still wired to be held unwired, and each that connects
/// again to be wired; a link `plan` removes is neither.
pub(super) fn judge_the_links_each_type_choice_reaches(
    graph: &mut Graph,
    plan: &mut CompilationPlan,
) {
    let nodes = std::mem::take(&mut plan.nodes_whose_type_choices_are_judged_again);
    for node_run_id in &nodes {
        let judged_again =
            graph.judge_again_each_link_the_type_choices_of_the_node_reach(node_run_id);
        for link_id in judged_again.differing {
            let wired = graph
                .traversal()
                .e(&link_id)
                .first()
                .is_some_and(this_link_is_already_wired_or_awaiting_its_helpers_answer);
            if wired
                && !plan.links_to_remove.contains(&link_id)
                && !plan.links_to_hold_unwired.contains(&link_id)
            {
                plan.links_to_hold_unwired.push(link_id);
            }
        }
        for link_id in judged_again.connecting_again {
            if !plan.links_to_remove.contains(&link_id) && !plan.links_to_add.contains(&link_id) {
                plan.links_to_add.push(link_id);
            }
        }
    }
    plan.nodes_whose_type_choices_are_judged_again = nodes;
}

/// Close the channel of each of `links`, whose ends no longer carry types that connect, keeping
/// each in the graph in error, so it carries nothing.
pub(super) fn hold_each_link_unwired(graph: &mut Graph, links: &[LinkUniqueId]) -> Result<()> {
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

/// Whether `link_id` is held unwired since its ends no longer carry types that connect, stamping
/// it in error when it is, so no wiring opens it.
pub(super) fn stays_unwired_while_its_pin_types_differ(
    graph: &mut Graph,
    link_id: &LinkUniqueId,
) -> bool {
    let differs = graph
        .traversal()
        .e(link_id)
        .first()
        .is_some_and(|link| link.has::<LinkPinTypesDifferComponent>());
    if differs {
        stamp_the_link_in_error(graph, link_id);
    }
    differs
}

fn stamp_the_link_in_error(graph: &mut Graph, link_id: &LinkUniqueId) {
    if let Some(link) = graph.traversal_mut().e(link_id).first_mut() {
        link.insert(LinkStateComponent(LinkState::Error));
    }
}
