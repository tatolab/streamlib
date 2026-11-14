// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: GPL-3.0-only

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

    /// Hand a running processor a configuration, recorded on its graph node
    /// only once the processor takes it.
    UpdateProcessorConfig {
        processor_id: ProcessorUniqueId,
        config_to_apply: serde_json::Value,
    },

    /// Move a function node whose call crashed or stalled its reader's helper,
    /// and every function node downstream of it, out of that reader's islands
    /// into a helper process of its own.
    PromoteAFunctionNodeOutOfTheIslandsOfTheReaderItCrashed {
        function_node_id: ProcessorUniqueId,
        reader_id: ProcessorUniqueId,
        promotion_request_number: u64,
        reason: String,
    },
}

impl std::fmt::Display for PendingOperation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PendingOperation::AddProcessor(id) => write!(f, "AddProcessor({})", id),
            PendingOperation::RemoveProcessor(id) => write!(f, "RemoveProcessor({})", id),
            PendingOperation::AddLink(id) => write!(f, "AddLink({})", id),
            PendingOperation::RemoveLink(id) => write!(f, "RemoveLink({})", id),
            PendingOperation::UpdateProcessorConfig { processor_id, .. } => {
                write!(f, "UpdateProcessorConfig({})", processor_id)
            }
            PendingOperation::PromoteAFunctionNodeOutOfTheIslandsOfTheReaderItCrashed {
                function_node_id,
                reader_id,
                ..
            } => write!(
                f,
                "PromoteAFunctionNodeOutOfTheIslandsOfTheReaderItCrashed({function_node_id} out of {reader_id})"
            ),
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
}
