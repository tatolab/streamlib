// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use serde::{Deserialize, Serialize};

/// Metadata about a port (input or output).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PortInfo {
    pub name: String,
    /// Human-readable description declared alongside the port. Mirrors the
    /// field on [`crate::core::descriptors::PortDescriptor`].
    #[serde(default)]
    pub description: String,
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
    /// The pin type this port carries, resolved from what its node type declares, and its
    /// flow; `None` on a port whose type the code does not state exactly, which links by its
    /// family alone.
    #[serde(flatten)]
    pub pin: Option<crate::core::descriptors::ResolvedPin>,
    /// Whether a value set live on this input is kept across Stop and a restart of the runtime,
    /// or reset at Stop. Mirrors the field on [`crate::core::descriptors::PortDescriptor`].
    #[serde(
        default,
        skip_serializing_if = "crate::core::descriptors::NodeValueLifetime::is_kept"
    )]
    pub lifetime: crate::core::descriptors::NodeValueLifetime,
}

impl From<&crate::core::descriptors::PortDescriptor> for PortInfo {
    fn from(port: &crate::core::descriptors::PortDescriptor) -> Self {
        Self {
            name: port.name.clone(),
            description: port.description.clone(),
            delivery_profile: port.delivery_profile.clone(),
            borrow_quota: port.borrow_quota,
            audio_window: port.audio_window.clone(),
            family: port.family.clone(),
            pin_default: port.pin_default.clone(),
            pin: port
                .declared_pin()
                .and_then(crate::core::descriptors::PinDeclaration::into_resolved),
            lifetime: port.lifetime,
        }
    }
}

impl PortInfo {
    /// A port named `name` that declares only `family`, typed as any port declaring that family
    /// is.
    pub fn declaring_only_a_family(
        name: impl Into<String>,
        description: impl Into<String>,
        family: crate::core::descriptors::PortFamilyDeclaration,
    ) -> Self {
        Self::from(&crate::core::descriptors::PortDescriptor::new(
            name,
            description,
            true,
            family,
        ))
    }
}
