// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A link the stream's function node pool carries, found and taken out by the ends it was given
//! with: the graph holds it only as its two halves through the pool, so a lookup of one link
//! out of its source finds nothing.

use super::function_node_pool::the_links_carried_through_the_function_node_pool_of;
use crate::core::NodeRunId;
use crate::core::error::{Error, Result};
use crate::core::graph::{Graph, InputLinkPortRef, LinkUniqueId, OutputLinkPortRef};

/// The link the pool carries from `source`'s output `source_port` into `target`'s input
/// `target_port`, by the id it was given; `None` when no standing pair of halves carries one.
pub(crate) fn the_link_carried_through_the_function_node_pool_between(
    graph: &Graph,
    source: &NodeRunId,
    source_port: &str,
    target: &NodeRunId,
    target_port: &str,
) -> Option<LinkUniqueId> {
    the_links_carried_through_the_function_node_pool_of(graph)
        .into_iter()
        .find(|carried| {
            let into_the_pool = carried.into_the_function_node_pool;
            let out_of_the_pool = carried.out_of_the_function_node_pool;
            graph.stands(into_the_pool)
                && graph.stands(out_of_the_pool)
                && into_the_pool.source().node_run_id() == source
                && into_the_pool.source().port_name() == source_port
                && out_of_the_pool.to_port().node_run_id() == target
                && out_of_the_pool.to_port().port_name() == target_port
        })
        .map(|carried| carried.carried_link_id)
}

/// What a disconnect of `link_id` takes out: the ends it joins as it was given, and the links
/// the graph holds it as, itself alone or the two halves the pool carries it as.
pub(crate) fn the_links_a_disconnect_takes_out(
    graph: &Graph,
    link_id: &LinkUniqueId,
) -> Result<(OutputLinkPortRef, InputLinkPortRef, Vec<LinkUniqueId>)> {
    if let Some(link) = graph.traversal().e(link_id).first() {
        return Ok((
            link.source().clone(),
            link.to_port().clone(),
            vec![link_id.clone()],
        ));
    }
    the_links_carried_through_the_function_node_pool_of(graph)
        .into_iter()
        .find(|carried| &carried.carried_link_id == link_id)
        .map(|carried| {
            (
                carried.into_the_function_node_pool.source().clone(),
                carried.out_of_the_function_node_pool.to_port().clone(),
                vec![
                    carried.into_the_function_node_pool.id.clone(),
                    carried.out_of_the_function_node_pool.id.clone(),
                ],
            )
        })
        .ok_or_else(|| Error::NotFound(format!("Link '{link_id}' not found")))
}
