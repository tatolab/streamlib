// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use crate::core::graph::ProcessorUniqueId;

/// Processor config change (for hot-reload, future use).
#[derive(Debug, Clone)]
pub struct ProcessorConfigChange {
    pub id: ProcessorUniqueId,
    pub old_config_checksum: u64,
    pub new_config_checksum: u64,
}
