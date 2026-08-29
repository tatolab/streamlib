// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use serde::{Deserialize, Serialize};

use super::PortKind;

/// Metadata about a port (input or output).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PortInfo {
    pub name: String,
    /// Human-readable description declared alongside the port. Mirrors the
    /// field on [`crate::core::descriptors::PortDescriptor`].
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub port_kind: PortKind,
    /// Delivery profile declared by this input port —
    /// `Some("newest" | "ordered")` on every input, `None`
    /// on an output. Mirrors the field on
    /// [`crate::core::descriptors::PortDescriptor`] so the compiler op
    /// can resolve a destination's delivery profile at wire time without
    /// locking the processor instance.
    #[serde(default)]
    pub delivery_profile: Option<String>,
}

impl From<&crate::core::descriptors::PortDescriptor> for PortInfo {
    fn from(port: &crate::core::descriptors::PortDescriptor) -> Self {
        Self {
            name: port.name.clone(),
            description: port.description.clone(),
            port_kind: PortKind::default(),
            delivery_profile: port.delivery_profile.clone(),
        }
    }
}
