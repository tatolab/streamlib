// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Pending operations for graph mutations.
//!
//! Instead of tracking a delta of what changed, we track explicit operations
//! that the user requested. Each operation is validated against the current
//! Graph + ECS state before execution.

use crate::core::graph::ProcessorId;
use crate::core::links::LinkId;

/// A pending graph mutation operation.
///
/// Operations are queued and executed at commit time. Each operation is
/// validated against current state before execution - if the referenced
/// processor/link no longer exists or is in an invalid state, the operation
/// is skipped or returns an error.
#[derive(Debug, Clone)]
pub enum PendingOperation {
    /// Add a processor that exists in the Graph but isn't running yet.
    AddProcessor(ProcessorId),

    /// Remove a processor that is currently running.
    RemoveProcessor(ProcessorId),

    /// Wire a link that exists in the Graph but isn't wired yet.
    AddLink(LinkId),

    /// Unwire and remove a link that is currently wired.
    RemoveLink(LinkId),

    /// Update a processor's configuration.
    UpdateProcessorConfig(ProcessorId),
}

impl PendingOperation {
    /// Get the processor ID if this operation involves a processor.
    pub fn processor_id(&self) -> Option<&ProcessorId> {
        match self {
            PendingOperation::AddProcessor(id) => Some(id),
            PendingOperation::RemoveProcessor(id) => Some(id),
            PendingOperation::UpdateProcessorConfig(id) => Some(id),
            PendingOperation::AddLink(_) | PendingOperation::RemoveLink(_) => None,
        }
    }

    /// Get the link ID if this operation involves a link.
    pub fn link_id(&self) -> Option<&LinkId> {
        match self {
            PendingOperation::AddLink(id) => Some(id),
            PendingOperation::RemoveLink(id) => Some(id),
            PendingOperation::AddProcessor(_)
            | PendingOperation::RemoveProcessor(_)
            | PendingOperation::UpdateProcessorConfig(_) => None,
        }
    }

    /// Check if this is an add operation.
    pub fn is_add(&self) -> bool {
        matches!(
            self,
            PendingOperation::AddProcessor(_) | PendingOperation::AddLink(_)
        )
    }

    /// Check if this is a remove operation.
    pub fn is_remove(&self) -> bool {
        matches!(
            self,
            PendingOperation::RemoveProcessor(_) | PendingOperation::RemoveLink(_)
        )
    }
}

impl std::fmt::Display for PendingOperation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PendingOperation::AddProcessor(id) => write!(f, "AddProcessor({})", id),
            PendingOperation::RemoveProcessor(id) => write!(f, "RemoveProcessor({})", id),
            PendingOperation::AddLink(id) => write!(f, "AddLink({})", id),
            PendingOperation::RemoveLink(id) => write!(f, "RemoveLink({})", id),
            PendingOperation::UpdateProcessorConfig(id) => {
                write!(f, "UpdateProcessorConfig({})", id)
            }
        }
    }
}

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
    pub fn remove_processor_operations(&mut self, processor_id: &ProcessorId) {
        self.operations
            .retain(|op| op.processor_id() != Some(processor_id));
    }

    /// Remove operations that reference the given link ID.
    pub fn remove_link_operations(&mut self, link_id: &LinkId) {
        self.operations.retain(|op| op.link_id() != Some(link_id));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pending_operation_display() {
        let op = PendingOperation::AddProcessor("proc_1".into());
        assert_eq!(format!("{}", op), "AddProcessor(proc_1)");
    }

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

    #[test]
    fn test_is_add_remove() {
        assert!(PendingOperation::AddProcessor("p".into()).is_add());
        assert!(PendingOperation::RemoveProcessor("p".into()).is_remove());
        assert!(!PendingOperation::AddProcessor("p".into()).is_remove());
        assert!(!PendingOperation::RemoveProcessor("p".into()).is_add());
    }
}
