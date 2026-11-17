// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use serde::{Deserialize, Serialize};

use super::{PinType, ProjectAssetMedia};
use crate::{
    ArrayOutputPortResidency, ChannelsOutputPortSampleRate, ChannelsOutputPortSampleType,
    ImageOutputPortFrameRate,
};

/// What a port declares about what it carries.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(
    feature = "contract-json-schema",
    derive(schemars_1::JsonSchema),
    schemars(crate = "schemars_1")
)]
#[serde(deny_unknown_fields)]
pub struct PinDeclaration {
    /// The type it carries, or how that type is found.
    pub pin_type: DeclaredPinType,
    /// How its messages are read.
    pub flow: PinFlow,
    /// What it states besides its type, none of which decides a link.
    #[serde(default, skip_serializing_if = "PinAnnotations::is_empty")]
    pub annotations: PinAnnotations,
    /// Names the port had before, each of which still finds it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub former_names: Vec<String>,
    /// Whether an input may hold no value and read as nothing.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub may_be_unset: bool,
}

impl PinDeclaration {
    /// A port of exactly `pin_type`, read in the type's default flow, stating nothing else.
    pub fn exactly(pin_type: PinType) -> Self {
        Self {
            flow: pin_type.default_flow(),
            pin_type: DeclaredPinType::Is { pin_type },
            annotations: PinAnnotations::default(),
            former_names: Vec::new(),
            may_be_unset: false,
        }
    }

    /// The type and flow a port of this declaration resolves to without its graph: the ones
    /// it declares, when it declares exactly one type; `None` when its type is found another way.
    pub fn into_resolved(self) -> Option<ResolvedPin> {
        match self.pin_type {
            DeclaredPinType::Is { pin_type } => Some(ResolvedPin {
                pin_type,
                flow: self.flow,
            }),
            DeclaredPinType::OneOf { .. }
            | DeclaredPinType::Any
            | DeclaredPinType::SameAsInput { .. }
            | DeclaredPinType::SetByInput { .. } => None,
        }
    }
}

/// The pin type a port of a graph carries, resolved, and how its messages are read.
// `Eq` because a graph's port info, which holds one, is compared whole.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
pub struct ResolvedPin {
    /// The type it carries. Its schema here is open JSON: only the control contract's schemars
    /// renders pin types.
    #[schemars(with = "serde_json::Value")]
    #[cfg_attr(feature = "utoipa", schema(value_type = Object))]
    pub pin_type: PinType,
    /// How its messages are read: `value`, `stream` or `trigger`.
    #[schemars(with = "String")]
    #[cfg_attr(feature = "utoipa", schema(value_type = String))]
    pub flow: PinFlow,
}

/// The pin type a port declares, or how it is found.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(
    feature = "contract-json-schema",
    derive(schemars_1::JsonSchema),
    schemars(crate = "schemars_1")
)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum DeclaredPinType {
    /// Exactly this type.
    Is {
        /// The type.
        pin_type: PinType,
    },
    /// Any one of these types.
    OneOf {
        /// The types it takes.
        pin_types: Vec<PinType>,
    },
    /// Every type. An input only.
    Any,
    /// The type of the named input's link.
    SameAsInput {
        /// The input whose type it takes.
        input_port_name: String,
    },
    /// The type the named pin type choice input holds.
    SetByInput {
        /// The input that chooses its type.
        input_name: String,
    },
}

/// How a pin's messages are read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(
    feature = "contract-json-schema",
    derive(schemars_1::JsonSchema),
    schemars(crate = "schemars_1")
)]
#[serde(rename_all = "snake_case")]
pub enum PinFlow {
    /// The newest message is read until a newer one arrives.
    Value,
    /// Each message is read as it arrives.
    Stream,
    /// Each message is an event, read in order.
    Trigger,
}

/// What a pin states besides its type. None of it decides a link.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(
    feature = "contract-json-schema",
    derive(schemars_1::JsonSchema),
    schemars(crate = "schemars_1")
)]
#[serde(deny_unknown_fields)]
pub struct PinAnnotations {
    /// The lowest and highest number it holds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub range: Option<PinNumberRange>,
    /// The step a control moves a number by.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step: Option<f64>,
    /// The unit a number is in, such as `Hz`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    /// What its value stands for, such as `frequency`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub meaning: Option<String>,
    /// The control that edits or shows it: one of the Studio's, or a module's qualified name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub control: Option<String>,
    /// The frame rate a Texture output's stream has, stated or read from a config key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frame_rate: Option<ImageOutputPortFrameRate>,
    /// The sample rate an Audio output's stream has, stated or read from a config key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sample_rate: Option<ChannelsOutputPortSampleRate>,
    /// The sample type an Audio output's stream has.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sample_type: Option<ChannelsOutputPortSampleType>,
    /// Where a Tensor output's values live.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub residency: Option<ArrayOutputPortResidency>,
    /// That a String holds the UUID of one of the project's assets, so a client offers the
    /// project's assets to pick from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_asset: Option<PinProjectAssetAnnotation>,
}

/// The project's assets a String holding an asset's UUID is picked from.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(
    feature = "contract-json-schema",
    derive(schemars_1::JsonSchema),
    schemars(crate = "schemars_1")
)]
#[serde(deny_unknown_fields)]
pub struct PinProjectAssetAnnotation {
    /// The media an asset must hold one of; empty, any asset of the project.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub media: Vec<ProjectAssetMedia>,
}

impl PinAnnotations {
    /// Whether it states nothing.
    pub fn is_empty(&self) -> bool {
        self == &Self::default()
    }
}

/// The lowest and highest number a pin holds; either may be left open.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(
    feature = "contract-json-schema",
    derive(schemars_1::JsonSchema),
    schemars(crate = "schemars_1")
)]
#[serde(deny_unknown_fields)]
pub struct PinNumberRange {
    /// The lowest; absent, no lower bound.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub minimum: Option<f64>,
    /// The highest; absent, no upper bound.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub maximum: Option<f64>,
}

/// Whether an input takes a wire, a stored value, or both.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(
    feature = "contract-json-schema",
    derive(schemars_1::JsonSchema),
    schemars(crate = "schemars_1")
)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
pub enum NodeInputMode {
    /// A link or a stored value, read each run.
    WireOrValue,
    /// A link only, read each run.
    WireOnly,
    /// A stored value only, never a link.
    SettingOnly {
        /// When the node reads it.
        #[serde(default)]
        read: SettingReadTime,
    },
}

/// When a node reads a setting-only input.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(
    feature = "contract-json-schema",
    derive(schemars_1::JsonSchema),
    schemars(crate = "schemars_1")
)]
#[serde(rename_all = "snake_case")]
pub enum SettingReadTime {
    /// Once, when the node is set up; a change rebuilds the node.
    #[default]
    AtSetup,
    /// Each run, with no rebuild. Runtime-owned nodes only.
    EachRun,
}
