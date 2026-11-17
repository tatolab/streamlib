// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::collections::HashSet;
use std::sync::Arc;

use parking_lot::Mutex;

use super::PendingOperation;
use crate::core::graph::{LinkUniqueId, NodeRunId};

/// Handle for logging operations to the compiler transaction.
pub(crate) struct CompilerTransactionHandle {
    inner: Arc<Mutex<Vec<PendingOperation>>>,
}

impl CompilerTransactionHandle {
    pub(crate) fn new(inner: Arc<Mutex<Vec<PendingOperation>>>) -> Self {
        Self { inner }
    }

    pub(crate) fn log(&self, op: PendingOperation) {
        self.inner.lock().push(op);
    }

    /// The config the last config update logged for `node` hands it, which it holds once the
    /// updates logged for it are applied; `None` when none is logged.
    pub(crate) fn the_config_last_logged_for(&self, node: &NodeRunId) -> Option<serde_json::Value> {
        self.inner
            .lock()
            .iter()
            .rev()
            .find_map(|operation| match operation {
                PendingOperation::UpdateNodeConfig {
                    node_run_id,
                    config_to_apply,
                } if node_run_id == node => Some(config_to_apply.clone()),
                _ => None,
            })
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
            | PendingOperation::RemoveNode(node_run_id)
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
