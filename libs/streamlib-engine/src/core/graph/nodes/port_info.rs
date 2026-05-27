// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use serde::{Deserialize, Serialize};
use streamlib_processor_schema::PortSchemaSpec;

use super::PortKind;

/// Metadata about a port (input or output).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PortInfo {
    pub name: String,
    pub data_type: PortSchemaSpec,
    #[serde(default)]
    pub port_kind: PortKind,
    /// Producer-side overflow policy declared by this input port —
    /// `Some("drop_oldest")` or `Some("block")`, or `None` for output
    /// ports / inputs that defer to the engine-wide default. Mirrors
    /// the field on [`crate::core::descriptors::PortDescriptor`] so
    /// the compiler op can resolve a destination's overflow at wire
    /// time without locking the processor instance.
    #[serde(default)]
    pub overflow: Option<String>,
}
