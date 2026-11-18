// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The answer `check_link` gives: what `connect` would answer for one link, made nowhere.

use serde::{Deserialize, Serialize};
use streamlib_node_schema::NodeClassImportPath;
use streamlib_node_schema::pin_type::{
    ConvertNodeToInsert, FreePinConversion, PinLinkOutcome, PinLinkVerdict, PinLinkWarning, PinType,
};

use super::ProjectActionRefusalOnTheWire;

/// `check_link`'s answer: whether and how `connect` would make the link, and the refusal it
/// would answer when it would not.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct LinkCheckResult {
    /// Whether and how `connect` would make the link.
    pub outcome: PinLinkOutcomeOnTheWire,
    /// The convert node `connect` would place between the two ends; present only when
    /// `outcome` is `connects_through_convert_node`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub convert_node: Option<ConvertNodeToInsertOnTheWire>,
    /// The whole refusal `connect` would answer, with its code; present only when `outcome` is
    /// `refused`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refusal: Option<ProjectActionRefusalOnTheWire>,
    /// What the link would do that a person may not expect; none of these blocks it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<PinLinkWarningOnTheWire>,
}

impl LinkCheckResult {
    /// The answer for a link `connect` would make as `verdict` judges it, or, with no verdict,
    /// as the families of two ends not both typed take it: as it is, warning of nothing.
    pub fn of_a_link_connect_makes(verdict: Option<&PinLinkVerdict>) -> Self {
        let Some(verdict) = verdict else {
            return Self {
                outcome: PinLinkOutcomeOnTheWire::Connects,
                convert_node: None,
                refusal: None,
                warnings: Vec::new(),
            };
        };
        Self {
            outcome: PinLinkOutcomeOnTheWire::of(&verdict.outcome),
            convert_node: match &verdict.outcome {
                PinLinkOutcome::ConnectsThroughConvertNode(convert_node) => {
                    Some(ConvertNodeToInsertOnTheWire::of(convert_node))
                }
                PinLinkOutcome::Connects
                | PinLinkOutcome::ConnectsWithFreeConversion(_)
                | PinLinkOutcome::Refused(_) => None,
            },
            refusal: None,
            warnings: verdict
                .warnings
                .iter()
                .map(PinLinkWarningOnTheWire::of)
                .collect(),
        }
    }

    /// The answer for a link `connect` would refuse with `refusal`.
    pub fn of_a_link_connect_refuses(refusal: ProjectActionRefusalOnTheWire) -> Self {
        Self {
            outcome: PinLinkOutcomeOnTheWire::Refused,
            convert_node: None,
            refusal: Some(refusal),
            warnings: Vec::new(),
        }
    }
}

/// Whether and how a link connects. The set grows: an outcome a client does not know reads as
/// `refused`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "json-schema", schemars(extend("x-may-grow" = true)))]
#[serde(rename_all = "snake_case")]
pub enum PinLinkOutcomeOnTheWire {
    /// The input takes what the output carries as it is.
    Connects,
    /// The input reads what the output carries as its own type, at no cost: Boolean, Integer
    /// and Float among themselves, a Vector into a Vector or a Color, one number into a Vector
    /// or a Color, or a Menu into a String.
    ConnectsWithFreeConversion,
    /// The two ends connect through the built-in convert node `convert_node` names, which
    /// `connect` places between them in the same edit, collapsed, linking each end to it.
    ConnectsThroughConvertNode,
    /// The link is not made; `refusal` says why.
    Refused,
}

impl PinLinkOutcomeOnTheWire {
    /// The wire spelling of `outcome`.
    fn of(outcome: &PinLinkOutcome) -> Self {
        match outcome {
            PinLinkOutcome::Connects => Self::Connects,
            PinLinkOutcome::ConnectsWithFreeConversion(_) => Self::ConnectsWithFreeConversion,
            PinLinkOutcome::ConnectsThroughConvertNode(_) => Self::ConnectsThroughConvertNode,
            PinLinkOutcome::Refused(_) => Self::Refused,
        }
    }
}

/// The node of one built-in conversion a link is made through.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct ConvertNodeToInsertOnTheWire {
    /// The node type that converts.
    pub node_type: NodeClassImportPath,
    /// The input the link's output feeds.
    pub input_port: String,
    /// The type that input takes.
    pub input_pin_type: PinType,
    /// The output that feeds the link's input.
    pub output_port: String,
    /// The type that output gives, under the settings `input_values` holds.
    pub output_pin_type: PinType,
    /// The settings the node holds, each by its input's name, picked for the link's input; absent
    /// when what its output gives follows no setting.
    #[serde(default, skip_serializing_if = "serde_json::Map::is_empty")]
    pub input_values: serde_json::Map<String, serde_json::Value>,
}

impl ConvertNodeToInsertOnTheWire {
    /// The wire form of `convert_node`, its output typed as the settings it picks make it.
    fn of(convert_node: &ConvertNodeToInsert) -> Self {
        let conversion = &convert_node.conversion;
        Self {
            node_type: conversion.node_type.clone(),
            input_port: conversion.input_port_name.clone(),
            input_pin_type: conversion.input_pin_type.clone(),
            output_port: conversion.output_port_name.clone(),
            output_pin_type: convert_node.output_pin_type.clone(),
            input_values: convert_node.input_values.clone(),
        }
    }
}

/// How the input reads what the output carries as its own type, at no cost. The set grows: a
/// conversion a client does not know reads as a free conversion it cannot name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "json-schema", schemars(extend("x-may-grow" = true)))]
#[serde(rename_all = "snake_case")]
pub enum FreePinConversionOnTheWire {
    /// Boolean, Integer and Float among themselves; a Float read as an Integer rounds half away
    /// from zero.
    BetweenNumbers,
    /// A Vector into one of another size, its components dropped or padded with 0.
    BetweenVectors,
    /// A Vector into a Color, x, y, z and w read as red, green, blue and alpha, alpha padded
    /// with 1.
    VectorIntoColor,
    /// One number into a Vector, read as every component.
    NumberSpreadOverVector,
    /// One number into a Color, read as red, green and blue, with an alpha of 1.
    NumberSpreadOverColor,
    /// A Menu into a String, its option name read as the text.
    MenuOptionNameAsString,
}

impl FreePinConversionOnTheWire {
    /// The wire form of `free_conversion`.
    pub fn of(free_conversion: FreePinConversion) -> Self {
        match free_conversion {
            FreePinConversion::BetweenNumbers => Self::BetweenNumbers,
            FreePinConversion::BetweenVectors => Self::BetweenVectors,
            FreePinConversion::VectorIntoColor => Self::VectorIntoColor,
            FreePinConversion::NumberSpreadOverVector => Self::NumberSpreadOverVector,
            FreePinConversion::NumberSpreadOverColor => Self::NumberSpreadOverColor,
            FreePinConversion::MenuOptionNameAsString => Self::MenuOptionNameAsString,
        }
    }
}

/// What a link does that a person may not expect, tagged by its `warning`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(tag = "warning", rename_all = "snake_case")]
pub enum PinLinkWarningOnTheWire {
    /// The two ends state different meanings for what they carry.
    MeaningsDiffer {
        /// The output's meaning.
        output_meaning: String,
        /// The input's meaning.
        input_meaning: String,
    },
    /// A member of two bundles states a different meaning at each end.
    MemberMeaningsDiffer {
        /// The member.
        member: String,
        /// The output member's meaning.
        output_meaning: String,
        /// The input member's meaning.
        input_meaning: String,
    },
    /// A Vector is read as a Color: x, y, z and w become red, green, blue and alpha.
    VectorReadAsAColor,
    /// The output may send Menu options the input does not list.
    MenuOptionsTheInputDoesNotList {
        /// Each such option, as the output names it.
        option_names: Vec<String>,
    },
    /// The output states no pixel format, so it may send one the input cannot take.
    TextureOutputStatesNoFormat,
}

impl PinLinkWarningOnTheWire {
    /// The wire form of `warning`.
    pub fn of(warning: &PinLinkWarning) -> Self {
        match warning {
            PinLinkWarning::MeaningsDiffer {
                output_meaning,
                input_meaning,
            } => Self::MeaningsDiffer {
                output_meaning: output_meaning.clone(),
                input_meaning: input_meaning.clone(),
            },
            PinLinkWarning::MemberMeaningsDiffer {
                member_name,
                output_meaning,
                input_meaning,
            } => Self::MemberMeaningsDiffer {
                member: member_name.clone(),
                output_meaning: output_meaning.clone(),
                input_meaning: input_meaning.clone(),
            },
            PinLinkWarning::VectorReadAsAColor => Self::VectorReadAsAColor,
            PinLinkWarning::MenuOptionsTheInputDoesNotList { option_names } => {
                Self::MenuOptionsTheInputDoesNotList {
                    option_names: option_names.clone(),
                }
            }
            PinLinkWarning::TextureOutputStatesNoFormat => Self::TextureOutputStatesNoFormat,
        }
    }
}
