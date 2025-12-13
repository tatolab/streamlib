// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use crate::core::compiler::link_config_change::LinkConfigChange;
use crate::core::compiler::processor_config_change::ProcessorConfigChange;
use crate::core::graph::{LinkUniqueId, ProcessorUniqueId};

/// Categorized pending operations for batch compilation.
#[derive(Debug, Default)]
pub struct OperationBatch {
    /// Processors in Graph but not yet spawned
    pub processors_to_add: Vec<ProcessorUniqueId>,
    /// Processors spawned but no longer in Graph
    pub processors_to_remove: Vec<ProcessorUniqueId>,
    /// Links in Graph but not yet wired
    pub links_to_add: Vec<LinkUniqueId>,
    /// Links wired but no longer in Graph
    pub links_to_remove: Vec<LinkUniqueId>,
    /// Processors with config changes (future use)
    pub processors_to_update: Vec<ProcessorConfigChange>,
    /// Links with config changes (future use)
    pub links_to_update: Vec<LinkConfigChange>,
}

impl OperationBatch {
    /// Check if there are no changes to apply.
    pub fn is_empty(&self) -> bool {
        self.processors_to_add.is_empty()
            && self.processors_to_remove.is_empty()
            && self.links_to_add.is_empty()
            && self.links_to_remove.is_empty()
            && self.processors_to_update.is_empty()
            && self.links_to_update.is_empty()
    }

    /// Total number of changes.
    pub fn change_count(&self) -> usize {
        self.processors_to_add.len()
            + self.processors_to_remove.len()
            + self.links_to_add.len()
            + self.links_to_remove.len()
            + self.processors_to_update.len()
            + self.links_to_update.len()
    }
}
