// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use crate::core::compiler::PendingOperation;
use crate::core::graph::{LinkUniqueId, ProcessorUniqueId};

/// Queue of pending operations to be executed at commit time.
#[derive(Debug, Default)]
pub struct PendingOperationQueue {
    operations: Vec<PendingOperation>,
}

impl PendingOperationQueue {
    /// Create a new empty queue.
    pub fn new() -> Self {
        Self {
            operations: Vec::new(),
        }
    }

    /// Push an operation onto the queue.
    pub fn push(&mut self, op: PendingOperation) {
        self.operations.push(op);
    }

    /// Check if the queue is empty.
    pub fn is_empty(&self) -> bool {
        self.operations.is_empty()
    }

    /// Get the number of pending operations.
    pub fn len(&self) -> usize {
        self.operations.len()
    }

    /// Take all operations, leaving the queue empty.
    pub fn take_all(&mut self) -> Vec<PendingOperation> {
        std::mem::take(&mut self.operations)
    }

    /// Iterate over pending operations.
    pub fn iter(&self) -> impl Iterator<Item = &PendingOperation> {
        self.operations.iter()
    }

    /// Clear all pending operations.
    pub fn clear(&mut self) {
        self.operations.clear();
    }

    /// Remove operations that reference the given processor ID.
    ///
    /// This is useful when a processor is removed - we should also remove
    /// any pending operations that reference it.
    pub fn remove_processor_operations(&mut self, processor_id: &ProcessorUniqueId) {
        self.operations
            .retain(|op| op.processor_id() != Some(processor_id));
    }

    /// Remove operations that reference the given link ID.
    pub fn remove_link_operations(&mut self, link_id: &LinkUniqueId) {
        self.operations.retain(|op| op.link_id() != Some(link_id));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pending_operation_queue() {
        let mut queue = PendingOperationQueue::new();
        assert!(queue.is_empty());

        queue.push(PendingOperation::AddProcessor("proc_1".into()));
        queue.push(PendingOperation::AddProcessor("proc_2".into()));
        assert_eq!(queue.len(), 2);

        let ops = queue.take_all();
        assert_eq!(ops.len(), 2);
        assert!(queue.is_empty());
    }

    #[test]
    fn test_remove_processor_operations() {
        let mut queue = PendingOperationQueue::new();
        queue.push(PendingOperation::AddProcessor("proc_1".into()));
        queue.push(PendingOperation::AddProcessor("proc_2".into()));
        queue.push(PendingOperation::UpdateProcessorConfig("proc_1".into()));

        queue.remove_processor_operations(&"proc_1".into());

        assert_eq!(queue.len(), 1);
        assert!(matches!(
            queue.iter().next(),
            Some(PendingOperation::AddProcessor(id)) if id == "proc_2"
        ));
    }
}
