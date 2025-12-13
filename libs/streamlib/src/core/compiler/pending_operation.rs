// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use crate::core::graph::{LinkUniqueId, ProcessorUniqueId};

/// A pending graph mutation operation.
///
/// Operations are queued and executed at commit time. Each operation is
/// validated against current state before execution - if the referenced
/// processor/link no longer exists or is in an invalid state, the operation
/// is skipped or returns an error.
#[derive(Debug, Clone)]
pub enum PendingOperation {
    /// Add a processor that exists in the Graph but isn't running yet.
    AddProcessor(ProcessorUniqueId),

    /// Remove a processor that is currently running.
    RemoveProcessor(ProcessorUniqueId),

    /// Wire a link that exists in the Graph but isn't wired yet.
    AddLink(LinkUniqueId),

    /// Unwire and remove a link that is currently wired.
    RemoveLink(LinkUniqueId),

    /// Update a processor's configuration.
    UpdateProcessorConfig(ProcessorUniqueId),
}

impl PendingOperation {
    /// Get the processor ID if this operation involves a processor.
    pub fn processor_id(&self) -> Option<&ProcessorUniqueId> {
        match self {
            PendingOperation::AddProcessor(id) => Some(id),
            PendingOperation::RemoveProcessor(id) => Some(id),
            PendingOperation::UpdateProcessorConfig(id) => Some(id),
            PendingOperation::AddLink(_) | PendingOperation::RemoveLink(_) => None,
        }
    }

    /// Get the link ID if this operation involves a link.
    pub fn link_id(&self) -> Option<&LinkUniqueId> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pending_operation_display() {
        let op = PendingOperation::AddProcessor("proc_1".into());
        assert_eq!(format!("{}", op), "AddProcessor(proc_1)");
    }

    #[test]
    fn test_is_add_remove() {
        assert!(PendingOperation::AddProcessor("p".into()).is_add());
        assert!(PendingOperation::RemoveProcessor("p".into()).is_remove());
        assert!(!PendingOperation::AddProcessor("p".into()).is_remove());
        assert!(!PendingOperation::RemoveProcessor("p".into()).is_add());
    }
}
