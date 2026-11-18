// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use serde::{Deserialize, Serialize};

/// Metadata about a port (input or output).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
    /// Whether this input takes a wire, a stored value, or both; `None` for an input port that
    /// takes a wire, and on every output port. Mirrors the field on
    /// [`crate::core::descriptors::PortDescriptor`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_mode: Option<crate::core::descriptors::NodeInputMode>,
    /// The lowest and highest number this input takes, as its pin declares them; `None` when it
    /// declares no range.
    #[serde(skip)]
    pub range: Option<crate::core::descriptors::PinNumberRange>,
}

// A range's bounds are never NaN, so a port compares equal to itself.
impl Eq for PortInfo {}

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
            range: declared_pin
                .as_ref()
                .and_then(|declared| declared.annotations.range),
            pin: declared_pin.and_then(crate::core::descriptors::PinDeclaration::into_resolved),
            lifetime: port.lifetime,
            input_mode: port.input_mode,
        }
    }
}

impl PortInfo {
    /// Whether a link may end at this input: every input but a setting.
    pub fn takes_a_wire(&self) -> bool {
        !matches!(
            self.input_mode,
            Some(crate::core::descriptors::NodeInputMode::SettingOnly { .. })
        )
    }

    /// Whether this input is a setting a node reads each run, whose value is set live apart
    /// from the graph.
    pub fn is_a_setting_read_each_run(&self) -> bool {
        matches!(
            self.input_mode,
            Some(crate::core::descriptors::NodeInputMode::SettingOnly {
                read: crate::core::descriptors::SettingReadTime::EachRun
            })
        )
    }

    /// The value `value` taken as this input's stored value, or what it takes instead, by the
    /// pin it declares; `None` for a port that names no pin type.
    pub fn check_value(
        &self,
        value: &serde_json::Value,
    ) -> Option<Result<(), crate::core::descriptors::PinValueNotTaken>> {
        let pin = self.pin.as_ref()?;
        if value.is_null() && self.may_be_unset {
            return Some(Ok(()));
        }
        Some(pin.pin_type.check_value(value))
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
