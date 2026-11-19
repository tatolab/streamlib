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
    /// The pin type this port's node type declares, or how its graph finds it, and its flow;
    /// `None` on a port that declares no pin. Not rendered here: `graph` serves the type it
    /// resolves to.
    #[serde(skip)]
    pub declared_pin_type: Option<PinTypeAsDeclared>,
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
    /// declares no range. Not rendered here: the pin declaration a client reads carries it.
    #[serde(skip)]
    pub range: Option<InputNumberRange>,
    /// What this port's value stands for, as its pin declares it, such as `frequency`; `None`
    /// when it declares none. Not rendered here: the pin declaration a client reads carries it.
    #[serde(skip)]
    pub meaning: Option<String>,
}

/// The pin type a port's node type declares, or how its graph finds it: exactly one, one of
/// several, any, the type linked into an input, or the type a setting chooses; and its flow.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PinTypeAsDeclared {
    /// The type, or how it is found.
    pub pin_type: crate::core::descriptors::DeclaredPinType,
    /// How its messages are read.
    pub flow: crate::core::descriptors::PinFlow,
}

/// The lowest and highest number an input takes, compared bound by bound bit for bit, so a port
/// compares equal to itself.
#[derive(Debug, Clone, Copy)]
pub struct InputNumberRange(pub crate::core::descriptors::PinNumberRange);

impl InputNumberRange {
    /// Each bound's bits, the form two ranges are compared in.
    fn bounds_as_bits(&self) -> (Option<u64>, Option<u64>) {
        (
            self.0.minimum.map(f64::to_bits),
            self.0.maximum.map(f64::to_bits),
        )
    }
}

impl PartialEq for InputNumberRange {
    fn eq(&self, other: &Self) -> bool {
        self.bounds_as_bits() == other.bounds_as_bits()
    }
}

impl Eq for InputNumberRange {}

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
                .and_then(|declared| declared.annotations.range)
                .map(InputNumberRange),
            meaning: declared_pin
                .as_ref()
                .and_then(|declared| declared.annotations.meaning.clone()),
            declared_pin_type: declared_pin.as_ref().map(|declared| PinTypeAsDeclared {
                pin_type: declared.pin_type.clone(),
                flow: declared.flow,
            }),
            pin: declared_pin.and_then(crate::core::descriptors::PinDeclaration::into_resolved),
            lifetime: port.lifetime,
            input_mode: port.input_mode,
        }
    }
}

impl PortInfo {
    /// Whether a link may end at this input: every input but a setting.
    pub fn takes_a_wire(&self) -> bool {
        self.input_mode
            .is_none_or(crate::core::descriptors::NodeInputMode::takes_a_wire)
    }

    /// Whether this port is a Trigger, whose messages each carry an event and no value.
    pub fn is_a_trigger(&self) -> bool {
        self.pin
            .as_ref()
            .is_some_and(|pin| pin.flow == crate::core::descriptors::PinFlow::Trigger)
    }

    /// Whether this input is a setting a node reads each run, whose value is set live apart
    /// from the graph.
    pub fn is_a_setting_read_each_run(&self) -> bool {
        self.input_mode
            .is_some_and(crate::core::descriptors::NodeInputMode::is_a_setting_read_each_run)
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

impl PortInfo {
    /// Whether `other` is the input of the same name that takes the values this one takes: its
    /// family, its pin type and how its graph finds it, its range, its input mode, its lifetime
    /// and whether it may be unset, whatever its description.
    pub(crate) fn takes_the_values_it_takes(&self, other: &PortInfo) -> bool {
        self.name == other.name
            && self.family == other.family
            && self.pin == other.pin
            && self.declared_pin_type == other.declared_pin_type
            && self.range == other.range
            && self.input_mode == other.input_mode
            && self.lifetime == other.lifetime
            && self.may_be_unset == other.may_be_unset
    }
}
