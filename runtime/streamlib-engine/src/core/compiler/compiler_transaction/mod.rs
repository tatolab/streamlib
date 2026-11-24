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

    pub(crate) fn retract_every_operation_naming(
        &self,
        nodes: &HashSet<NodeRunId>,
        links: &HashSet<LinkUniqueId>,
    ) {
        self.inner.lock().retain(|operation| match operation {
            PendingOperation::AddNode(node_run_id)
            | PendingOperation::RemoveNode(node_run_id, _)
            | PendingOperation::RebuildNode(node_run_id) => !nodes.contains(node_run_id),
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
