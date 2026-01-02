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
    /// Display name override. If None, defaults to `name`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
}

impl ProcessorSpec {
    pub fn new(name: impl Into<String>, config: serde_json::Value) -> Self {
        Self {
            name: name.into(),
            config,
            display_name: None,
        }
    }

    /// Set a custom display name for this processor.
    pub fn with_display_name(mut self, display_name: impl Into<String>) -> Self {
        self.display_name = Some(display_name.into());
        self
    }
}
