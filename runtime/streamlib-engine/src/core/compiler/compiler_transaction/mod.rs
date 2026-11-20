// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use parking_lot::Mutex;

use super::PendingOperation;
use crate::core::graph::{Graph, LinkUniqueId, NodeRunId};

/// The config each node holds once the commit in progress applies the config update it took
/// for the node, by node; empty while no commit is in progress.
pub(crate) type ConfigsACommitIsApplying = Arc<Mutex<HashMap<NodeRunId, serde_json::Value>>>;

/// Handle for logging operations to the compiler transaction.
pub(crate) struct CompilerTransactionHandle {
    inner: Arc<Mutex<Vec<PendingOperation>>>,
    configs_a_commit_is_applying: ConfigsACommitIsApplying,
}

impl CompilerTransactionHandle {
    pub(crate) fn new(
        inner: Arc<Mutex<Vec<PendingOperation>>>,
        configs_a_commit_is_applying: ConfigsACommitIsApplying,
    ) -> Self {
        Self {
            inner,
            configs_a_commit_is_applying,
        }
    }

    /// The config `node` of `graph` holds once every config update logged or taken by the
    /// commit in progress is applied: the one last logged, else the one the commit in progress
    /// applies, else the one it holds; `None` for a node holding none.
    pub(crate) fn the_config_the_node_will_hold(
        &self,
        graph: &Graph,
        node: &NodeRunId,
    ) -> Option<serde_json::Value> {
        let transaction = self.inner.lock();
        transaction
            .iter()
            .rev()
            .find_map(|operation| match operation {
                PendingOperation::UpdateNodeConfig {
                    node_run_id,
                    config_to_apply,
                } if node_run_id == node => Some(config_to_apply.clone()),
                _ => None,
            })
            .or_else(|| self.configs_a_commit_is_applying.lock().get(node).cloned())
            .or_else(|| {
                graph
                    .traversal()
                    .v(node)
                    .first()
                    .and_then(|held| held.config.clone())
            })
    }

    pub(crate) fn log(&self, op: PendingOperation) {
        self.inner.lock().push(op);
    }

    /// Take out each config update logged for a node of `nodes`, in the order logged.
    pub(crate) fn take_each_config_update_of(
        &self,
        nodes: &HashSet<NodeRunId>,
    ) -> Vec<(NodeRunId, serde_json::Value)> {
        self.inner
            .lock()
            .extract_if(.., |operation| {
                matches!(
                    operation,
                    PendingOperation::UpdateNodeConfig { node_run_id, .. }
                        if nodes.contains(node_run_id)
                )
            })
            .filter_map(|operation| match operation {
                PendingOperation::UpdateNodeConfig {
                    node_run_id,
                    config_to_apply,
                } => Some((node_run_id, config_to_apply)),
                _ => None,
            })
            .collect()
    }

    pub(crate) fn retract_every_operation_naming(
        &self,
        nodes: &HashSet<NodeRunId>,
        links: &HashSet<LinkUniqueId>,
    ) {
        self.inner.lock().retain(|operation| match operation {
            PendingOperation::AddNode(node_run_id)
            | PendingOperation::RemoveNode(node_run_id, _)
            | PendingOperation::RebuildNode(node_run_id)
            | PendingOperation::UpdateNodeConfig { node_run_id, .. } => {
                !nodes.contains(node_run_id)
            }
            PendingOperation::AddLink(link_id) | PendingOperation::RemoveLink(link_id) => {
                !links.contains(link_id)
            }
            PendingOperation::PromoteAFunctionNodeOutOfTheIslandsOfTheReaderItCrashed {
                function_node_id,
                reader_id,
                ..
            } => !nodes.contains(function_node_id) && !nodes.contains(reader_id),
        });
    }
}

#[cfg(test)]
mod tests;
