// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use crate::core::graph::{LinkUniqueId, NodeRunId};

#[derive(Debug, Clone)]
pub(crate) enum PendingOperation {
    AddNode(NodeRunId),

    RemoveNode(NodeRunId),

    /// Wire a link that exists in the Graph but isn't wired yet.
    AddLink(LinkUniqueId),

    /// Unwire and remove a link that is currently wired.
    RemoveLink(LinkUniqueId),

    UpdateNodeConfig {
        node_run_id: NodeRunId,
        config_to_apply: serde_json::Value,
    },

    /// Move a function node whose call crashed or stalled its reader's helper,
    /// and every function node downstream of it, out of that reader's islands
    /// into a helper process of its own.
    PromoteAFunctionNodeOutOfTheIslandsOfTheReaderItCrashed {
        function_node_id: NodeRunId,
        reader_id: NodeRunId,
        promotion_request_number: u64,
        reason: String,
    },
}

impl std::fmt::Display for PendingOperation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PendingOperation::AddNode(id) => write!(f, "AddNode({})", id),
            PendingOperation::RemoveNode(id) => write!(f, "RemoveNode({})", id),
            PendingOperation::AddLink(id) => write!(f, "AddLink({})", id),
            PendingOperation::RemoveLink(id) => write!(f, "RemoveLink({})", id),
            PendingOperation::UpdateNodeConfig { node_run_id, .. } => {
                write!(f, "UpdateNodeConfig({})", node_run_id)
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
        let op = PendingOperation::AddNode("proc_1".into());
        assert_eq!(format!("{}", op), "AddNode(proc_1)");
    }
}
