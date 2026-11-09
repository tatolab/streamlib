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
    /// Borrow quota declared by this input port, or `None` for the engine's
    /// default. Mirrors the field on
    /// [`crate::core::descriptors::PortDescriptor`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub borrow_quota: Option<usize>,
    /// Window contract declared by this audio input port, or `None`. Mirrors
    /// the field on [`crate::core::descriptors::PortDescriptor`]; absent from
    /// the rendering entirely on a port that declares none, so a contract-less
    /// port renders exactly what it always did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_window: Option<crate::core::descriptors::AudioWindowContract>,
    /// The port family this port declares. Mirrors the field on
    /// [`crate::core::descriptors::PortDescriptor`].
    pub family: crate::core::descriptors::PortFamilyDeclaration,
    /// The value this input reads while nothing is linked into it and no pin
    /// value is set on its node. Mirrors the field on
    /// [`crate::core::descriptors::PortDescriptor`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pin_default: Option<serde_json::Value>,
}

impl From<&crate::core::descriptors::PortDescriptor> for PortInfo {
    fn from(port: &crate::core::descriptors::PortDescriptor) -> Self {
        Self {
            name: port.name.clone(),
            description: port.description.clone(),
            port_kind: PortKind::default(),
            delivery_profile: port.delivery_profile.clone(),
            borrow_quota: port.borrow_quota,
            audio_window: port.audio_window.clone(),
            family: port.family.clone(),
            pin_default: port.pin_default.clone(),
        }
    }
}
