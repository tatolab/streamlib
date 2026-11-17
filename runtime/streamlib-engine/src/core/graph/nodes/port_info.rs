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
    /// Names this port had before, each of which still finds it. Mirrors the declaration's on
    /// [`crate::core::descriptors::PortDescriptor`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub former_names: Vec<String>,
    /// Whether this input may hold no value, and then reads as nothing. Mirrors the
    /// declaration's on [`crate::core::descriptors::PortDescriptor`].
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub may_be_unset: bool,
}

impl From<&crate::core::descriptors::PortDescriptor> for PortInfo {
    fn from(port: &crate::core::descriptors::PortDescriptor) -> Self {
        let declared_pin = port.declared_pin();
        Self {
            name: port.name.clone(),
            description: port.description.clone(),
            delivery_profile: port.delivery_profile.clone(),
            borrow_quota: port.borrow_quota,
            audio_window: port.audio_window.clone(),
            family: port.family.clone(),
            pin_default: port.pin_default.clone(),
            former_names: declared_pin
                .as_ref()
                .map(|declared| declared.former_names.clone())
                .unwrap_or_default(),
            may_be_unset: declared_pin
                .as_ref()
                .is_some_and(|declared| declared.may_be_unset),
            pin: declared_pin.and_then(crate::core::descriptors::PinDeclaration::into_resolved),
            lifetime: port.lifetime,
        }
    }
}

impl PortInfo {
    /// The port of `ports` that `name` finds: the port so named, else the one that lists `name`
    /// as a former name. Every lookup of a port by a name a caller, a document or an older edit
    /// gives goes through here, so a renamed port is found under the name it had.
    pub fn the_one_a_name_finds<'port>(
        ports: &'port [PortInfo],
        name: &str,
    ) -> Option<&'port PortInfo> {
        ports.iter().find(|port| port.name == name).or_else(|| {
            ports
                .iter()
                .find(|port| port.former_names.iter().any(|former| former == name))
        })
    }

    /// `values`, each value by input name, with each one held under a name one of `inputs` had
    /// moved to that input's name; a value already held under that name stays, and a key no
    /// input names is kept as it is.
    pub fn input_values_under_the_current_names(
        inputs: &[PortInfo],
        values: serde_json::Map<String, serde_json::Value>,
    ) -> serde_json::Map<String, serde_json::Value> {
        let mut under_former_names = Vec::new();
        let mut current = serde_json::Map::with_capacity(values.len());
        for (name, value) in values {
            match Self::the_one_a_name_finds(inputs, &name) {
                Some(input) if input.name != name => {
                    under_former_names.push((input.name.clone(), value));
                }
                _ => {
                    current.insert(name, value);
                }
            }
        }
        for (name, value) in under_former_names {
            current.entry(name).or_insert(value);
        }
        current
    }

    /// The value `value` taken as this input's stored value, or what it takes instead, by the
    /// pin it declares; `None` for a port that names no pin type.
    pub fn check_value(
        &self,
        value: &serde_json::Value,
    ) -> Option<Result<(), crate::core::descriptors::PinValueNotTaken>> {
        let pin = self.pin.as_ref()?;
        let declared = crate::core::descriptors::PinDeclaration {
            may_be_unset: self.may_be_unset,
            ..crate::core::descriptors::PinDeclaration::exactly(pin.pin_type.clone())
        };
        Some(declared.check_value(value))
    }

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
