// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use serde_json::Value as JsonValue;

use crate::core::execution::ProcessExecution;

/// Marks a function node moved out of its readers' islands into a helper
/// process of its own, which drives it the way `process_execution` says.
///
/// Stored but never rendered: `graph` reads the move off the node's metrics.
pub struct FunctionNodePromotedToItsOwnProcessComponent {
    /// How the node's own helper drives it: its reader's interval, or the
    /// promotion default for a reader with none.
    pub process_execution: ProcessExecution,
}

/// What moved a function node out of its readers' islands, as `graph` renders
/// it under the node's `metrics`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FunctionNodePromotionOutOfItsReadersIslands {
    /// The name in the graph of the reader whose helper the node crashed or
    /// stalled.
    pub after_crashing_the_helper_of: String,
    /// Why that helper was lost, as its death was reported.
    pub reason: String,
}

impl FunctionNodePromotionOutOfItsReadersIslands {
    /// The `metrics` entry of a promoted function node.
    pub fn to_json(&self) -> JsonValue {
        serde_json::json!({
            "promotions": 1,
            "after_crashing_the_helper_of": self.after_crashing_the_helper_of,
            "reason": self.reason,
        })
    }
}
