// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use crate::core::graph::{LinkUniqueId, ProcessorUniqueId};

/// Categorized operations ready for execution.
///
/// Built from [`PendingOperation`]s after validation and dependency analysis.
#[derive(Debug, Default)]
pub(super) struct CompilationPlan {
    pub(super) processors_to_add: Vec<ProcessorUniqueId>,
    pub(super) processors_to_remove: Vec<ProcessorUniqueId>,
    pub(super) links_to_add: Vec<LinkUniqueId>,
    pub(super) links_to_remove: Vec<LinkUniqueId>,
    pub(super) config_updates: Vec<ProcessorUniqueId>,
}

impl CompilationPlan {
    /// Returns true if there are no operations to execute.
    pub(super) fn is_empty(&self) -> bool {
        self.processors_to_add.is_empty()
            && self.processors_to_remove.is_empty()
            && self.links_to_add.is_empty()
            && self.links_to_remove.is_empty()
            && self.config_updates.is_empty()
    }
}
