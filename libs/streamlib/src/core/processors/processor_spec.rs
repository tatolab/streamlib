// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use serde::{Deserialize, Serialize};

/// Specification for creating a processor.
///
/// Contains only what the user provides: processor name and configuration.
/// Internal details (id, ports) are resolved by the runtime.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessorSpec {
    /// Processor name (matches registered name in PROCESSOR_REGISTRY).
    pub name: String,
    /// Configuration as JSON value.
    pub config: serde_json::Value,
}

impl ProcessorSpec {
    pub fn new(name: impl Into<String>, config: serde_json::Value) -> Self {
        Self {
            name: name.into(),
            config,
        }
    }
}
