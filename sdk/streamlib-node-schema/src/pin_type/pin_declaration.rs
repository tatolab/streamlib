// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use serde::{Deserialize, Serialize};

use super::PinType;

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
    /// The frame rate a Texture output's stream has.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frame_rate: Option<PinFrameRateAnnotation>,
    /// The sample rate an Audio output's stream has.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sample_rate: Option<PinSampleRateAnnotation>,
    /// The sample type an Audio output's stream has.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sample_type: Option<PinAudioSampleType>,
    /// Where a Tensor output's values live.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub residency: Option<PinTensorResidency>,
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

/// The frame rate a Texture output states, or the setting it follows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(
    feature = "contract-json-schema",
    derive(schemars_1::JsonSchema),
    schemars(crate = "schemars_1")
)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum PinFrameRateAnnotation {
    /// Frames per second, as a rational number.
    Stated { numerator: u32, denominator: u32 },
    /// The value of the node's input of this name.
    FromSetting { setting: String },
}

/// The sample rate an Audio output states, or the setting it follows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(
    feature = "contract-json-schema",
    derive(schemars_1::JsonSchema),
    schemars(crate = "schemars_1")
)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum PinSampleRateAnnotation {
    /// Samples per second.
    Stated(u32),
    /// The value of the node's input of this name.
    FromSetting { setting: String },
}

/// The type of each sample of an Audio stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(
    feature = "contract-json-schema",
    derive(schemars_1::JsonSchema),
    schemars(crate = "schemars_1")
)]
#[serde(rename_all = "snake_case")]
pub enum PinAudioSampleType {
    /// 32-bit float samples.
    F32,
    /// 64-bit float samples.
    F64,
}

/// Where a Tensor's values live.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(
    feature = "contract-json-schema",
    derive(schemars_1::JsonSchema),
    schemars(crate = "schemars_1")
)]
#[serde(rename_all = "snake_case")]
pub enum PinTensorResidency {
    /// In GPU memory.
    Gpu,
    /// In CPU memory.
    Cpu,
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
