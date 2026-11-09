// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::collections::HashSet;
use std::sync::Arc;

use parking_lot::Mutex;

use super::PendingOperation;
use crate::core::graph::{LinkUniqueId, ProcessorUniqueId};

/// Handle for logging operations to the compiler transaction.
pub struct CompilerTransactionHandle {
    inner: Arc<Mutex<Vec<PendingOperation>>>,
}

impl CompilerTransactionHandle {
    pub(crate) fn new(inner: Arc<Mutex<Vec<PendingOperation>>>) -> Self {
        Self { inner }
    }

    pub fn log(&self, op: PendingOperation) {
        self.inner.lock().push(op);
    }

    /// Take back each logged operation that names one of `processors` or `links`, for a node
    /// or link taken out of the graph before any commit built it.
    pub(crate) fn retract_every_operation_naming(
        &self,
        processors: &HashSet<ProcessorUniqueId>,
        links: &HashSet<LinkUniqueId>,
    ) {
        self.inner.lock().retain(|operation| match operation {
            PendingOperation::AddProcessor(processor_id)
            | PendingOperation::RemoveProcessor(processor_id)
            | PendingOperation::UpdateProcessorConfig { processor_id, .. } => {
                !processors.contains(processor_id)
            }
            PendingOperation::AddLink(link_id) | PendingOperation::RemoveLink(link_id) => {
                !links.contains(link_id)
            }
            PendingOperation::PromoteAFunctionNodeOutOfTheIslandsOfTheReaderItCrashed {
                function_node_id,
                reader_id,
                ..
            } => !processors.contains(function_node_id) && !processors.contains(reader_id),
        });
    }
}
