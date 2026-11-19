// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The answers `check_link` and `check_link_candidates` give: what `connect` would answer for
//! one link, or for each of many, made nowhere.

use serde::{Deserialize, Serialize};
use streamlib_node_schema::NodeClassImportPath;
use streamlib_node_schema::pin_type::{
    ConvertNodeToInsert, PinLinkOutcome, PinLinkVerdict, PinLinkWarning, PinType,
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

/// One end `check_link_candidates` judges its placed end against, of the other direction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum LinkCandidateEnd {
    /// A port of a node the stream's graph holds.
    PlacedPort {
        /// The node, by the UUID `graph` lists as its `id`.
        node: String,
        /// The port's name, as `graph` lists it under the node's `ports`.
        port: String,
    },
    /// A port of a node type no node of the graph has yet, judged as a node of the type an
    /// `add_node` with no input values would place.
    NodeTypePort {
        /// The node type, by the import path `add_node` takes as its `type`.
        node_type: String,
        /// The port's name, as the node type declares it.
        port: String,
    },
}

/// One candidate end and what `check_link` would answer for the link between it and the placed
/// end.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct LinkCandidateChecked {
    /// The candidate end.
    pub end: LinkCandidateEnd,
    /// What `check_link` would answer for the link.
    pub check: LinkCheckResult,
}

/// One candidate end whose link `check_link` would not check yet, and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct LinkCandidateNotChecked {
    /// The candidate end.
    pub end: LinkCandidateEnd,
    /// Why the link is not checked yet, in the runtime's words.
    pub why: String,
}

/// `check_link_candidates`'s answer: for each candidate end, in the order judged, what
/// `check_link` would answer for its link with the placed end.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct LinkCandidatesCheckResult {
    /// Each candidate checked; one `connect` would refuse is listed only when the request set
    /// `include_refused`.
    pub candidates: Vec<LinkCandidateChecked>,
    /// Each candidate whose link is not checked yet, as `check_link` would not check it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub not_checked: Vec<LinkCandidateNotChecked>,
}

/// What a link made between two typed ends converts and warns of, as the judgement of those
/// ends says: the marks a link carries in `graph` and in a transaction's answer.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
pub struct PinLinkMarksOnTheWire {
    /// `true` when the input reads what the output carries as its own type at no cost: Boolean,
    /// Integer and Float among themselves, a Vector into a Vector or a Color, one number into a
    /// Vector or a Color, or a Menu into a String. Absent on every other link.
    #[serde(default, skip_serializing_if = "is_false")]
    pub free_conversion: bool,
    /// What the link does that a person may not expect, as the judgement of its two ends' pin
    /// types says; none of these blocks it. Absent when it warns of nothing.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<PinLinkWarningOnTheWire>,
}

fn is_false(value: &bool) -> bool {
    !*value
}

impl PinLinkMarksOnTheWire {
    /// The marks of a link `connect` makes as `verdict` judges it; with no verdict, a link with
    /// an end untyped, neither.
    pub fn of(verdict: Option<&PinLinkVerdict>) -> Self {
        let answered = LinkCheckResult::of_a_link_connect_makes(verdict);
        Self {
            free_conversion: answered.outcome
                == PinLinkOutcomeOnTheWire::ConnectsWithFreeConversion,
            warnings: answered.warnings,
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
    /// The two ends connect through the convert node `convert_node` names, placed between them.
    /// Not answered yet: `connect` refuses a link that needs a convert node, so such a link is
    /// answered `refused`.
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
    /// The type that output gives.
    pub output_pin_type: PinType,
}

impl ConvertNodeToInsertOnTheWire {
    /// The wire form of `convert_node`.
    fn of(convert_node: &ConvertNodeToInsert) -> Self {
        let conversion = &convert_node.conversion;
        Self {
            node_type: conversion.node_type.clone(),
            input_port: conversion.input_port_name.clone(),
            input_pin_type: conversion.input_pin_type.clone(),
            output_port: conversion.output_port_name.clone(),
            output_pin_type: conversion.output_pin_type.clone(),
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
    /// The candidate, a node type's port, finds its type from a link into its node or from a
    /// setting, so it is judged as taking the other end's type; a node of the type, once placed
    /// and given them, may still refuse the link.
    TypeFoundOnlyOncePlaced,
}

impl PinLinkWarningOnTheWire {
    /// The wire form of `warning`.
    fn of(warning: &PinLinkWarning) -> Self {
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
            PinLinkWarning::TypeFoundOnlyOncePlaced => Self::TypeFoundOnlyOncePlaced,
        }
    }
}
