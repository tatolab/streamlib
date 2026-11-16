// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use parking_lot::RwLock;

use crate::core::error::{Error, Result};
use crate::core::graph::{Graph, GraphNodeWithComponents, NodeInstanceComponent, NodeRunId};

#[must_use]
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum NodeConfigUpdateOutcome {
    TakenAndRecordedOnTheNode,
    NodeNoLongerInTheGraph,
}

pub(crate) fn apply_node_config_update(
    graph: &RwLock<Graph>,
    node_run_id: &NodeRunId,
    config_to_apply: serde_json::Value,
) -> Result<NodeConfigUpdateOutcome> {
    let node_instance = {
        let graph = graph.read();
        let Some(node) = graph.traversal().v(node_run_id).first() else {
            tracing::warn!("[CONFIG] Node {} not found in graph", node_run_id);
            return Ok(NodeConfigUpdateOutcome::NodeNoLongerInTheGraph);
        };
        node.get::<NodeInstanceComponent>()
            .map(|instance| instance.0.clone())
            .ok_or_else(|| {
                Error::NodeNotFound(format!(
                    "Node '{}' not found for config update",
                    node_run_id
                ))
            })?
    };

    node_instance.lock().apply_config_json(&config_to_apply)?;

    if let Some(node) = graph.write().traversal_mut().v(node_run_id).first_mut() {
        node.set_config(config_to_apply);
    }
    Ok(NodeConfigUpdateOutcome::TakenAndRecordedOnTheNode)
}

#[cfg(test)]
mod tests;
