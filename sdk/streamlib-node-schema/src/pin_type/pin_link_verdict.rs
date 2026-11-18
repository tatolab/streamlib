// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::fmt;

use super::{BundleMember, NodeInputMode, PinConversion, PinType};
use crate::NodeClassImportPath;
use crate::port_family::{
    CodecDirection, EncodedMedia, PortFamily, codec_nodes_placed_between,
    node_placed_between_families,
};
use crate::stated_lists::{the_ones_not_held, the_ones_not_taken};

/// One end of a link: the port, or one member of a bundle port, with its resolved type.
#[derive(Debug, Clone, Copy)]
pub struct PinLinkEnd<'end> {
    /// The node's name.
    pub node_name: &'end str,
    /// The port's name.
    pub port_name: &'end str,
    /// The member of a bundle port the link names; `None` for the whole port.
    pub member_name: Option<&'end str>,
    /// The type the end carries, resolved.
    pub pin_type: &'end PinType,
    /// What its value stands for, as its pin states it.
    pub meaning: Option<&'end str>,
}

/// The input end of a link, with whether the input takes a wire.
#[derive(Debug, Clone, Copy)]
pub struct PinLinkInputEnd<'end> {
    /// The end.
    pub end: PinLinkEnd<'end>,
    /// Whether the input takes a wire, a stored value, or both; `None` for one that takes a wire.
    pub input_mode: Option<NodeInputMode>,
}

/// What [`pin_link_verdict`] answers of a link: its outcome, and warnings that never block it.
#[derive(Debug, Clone, PartialEq)]
pub struct PinLinkVerdict {
    /// Whether and how the link connects.
    pub outcome: PinLinkOutcome,
    /// What the link does that a person may not expect.
    pub warnings: Vec<PinLinkWarning>,
}

/// Whether and how a link between two typed pins connects.
#[derive(Debug, Clone, PartialEq)]
pub enum PinLinkOutcome {
    /// The input takes what the output carries as it is.
    Connects,
    /// The input takes what the output carries as its own type, at no cost.
    ConnectsWithFreeConversion(FreePinConversion),
    /// The two ends connect through a node of one built-in conversion placed between them.
    ConnectsThroughConvertNode(ConvertNodeToInsert),
    /// The two ends do not connect.
    Refused(PinLinkRefusal),
}

/// Two differing types the verdict lets connect, each variant stating what the input reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FreePinConversion {
    /// Boolean, Integer and Float among themselves; a Float read as an Integer rounds half away.
    BetweenNumbers,
    /// A Vector into one of another size, its components to be dropped or padded with 0.
    BetweenVectors,
    /// A Vector into a Color, x, y, z and w to be red, green, blue and alpha, alpha padded with 1.
    VectorIntoColor,
    /// One number into a Vector, to be every component.
    NumberSpreadOverVector,
    /// One number into a Color, to be red, green and blue, with an alpha of 1.
    NumberSpreadOverColor,
    /// A Menu into a String, its option name to be the text.
    MenuOptionNameAsString,
}

/// The convert node a link needs, and the refusal of the link made without it.
#[derive(Debug, Clone, PartialEq)]
pub struct ConvertNodeToInsert {
    /// The built-in conversion that bridges the two ends.
    pub conversion: PinConversion,
    /// The refusal of the link made straight from one end to the other.
    pub refusal_without_it: PinLinkRefusal,
}

/// Why a link is refused; its words are the sentence a client shows, naming both ends.
#[derive(Debug, Clone, PartialEq)]
pub enum PinLinkRefusal {
    /// The input is a setting, which takes a stored value and never a wire.
    InputTakesNoWire {
        /// The node of the input.
        node_name: String,
        /// The input.
        input_name: String,
    },
    /// No rule connects the output's type to the input's.
    PinTypesDoNotConnect {
        /// The output end.
        output: PinLinkEndNamed,
        /// The input end.
        input: PinLinkEndNamed,
        /// Each node type stating a conversion that bridges the two ends.
        conversions_that_bridge: Vec<NodeClassImportPath>,
    },
}

impl PinLinkRefusal {
    /// The refusal of a link into input `input_name` of node `node_name`, a setting.
    pub fn input_takes_no_wire(node_name: &str, input_name: &str) -> Self {
        Self::InputTakesNoWire {
            node_name: node_name.to_owned(),
            input_name: input_name.to_owned(),
        }
    }
}

/// One end of a link as a refusal names it.
#[derive(Debug, Clone, PartialEq)]
pub struct PinLinkEndNamed {
    /// The node's name.
    pub node_name: String,
    /// The port's name.
    pub port_name: String,
    /// The member of a bundle port the link names.
    pub member_name: Option<String>,
    /// The type it carries.
    pub pin_type: PinType,
}

impl From<PinLinkEnd<'_>> for PinLinkEndNamed {
    fn from(end: PinLinkEnd<'_>) -> Self {
        Self {
            node_name: end.node_name.to_owned(),
            port_name: end.port_name.to_owned(),
            member_name: end.member_name.map(str::to_owned),
            pin_type: end.pin_type.clone(),
        }
    }
}

impl fmt::Display for PinLinkEndNamed {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "`{}.{}", self.node_name, self.port_name)?;
        if let Some(member_name) = &self.member_name {
            write!(formatter, ".{member_name}")?;
        }
        write!(formatter, "` ({})", self.pin_type)
    }
}

/// Each of `names` in backticks, joined by commas.
fn in_backticks<Name: fmt::Display>(names: impl IntoIterator<Item = Name>) -> String {
    names
        .into_iter()
        .map(|name| format!("`{name}`"))
        .collect::<Vec<_>>()
        .join(", ")
}

impl fmt::Display for PinLinkRefusal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InputTakesNoWire {
                node_name,
                input_name,
            } => write!(
                formatter,
                "a link enters input `{input_name}` of `{node_name}`, a setting, which takes no \
                 wire. To fix: set its value with `set_node_input_value`"
            ),
            Self::PinTypesDoNotConnect {
                output,
                input,
                conversions_that_bridge,
            } => {
                write!(
                    formatter,
                    "cannot link {output} to {input}: a {} output does not connect to a {} \
                     input",
                    output.pin_type, input.pin_type
                )?;
                match conversions_that_bridge.as_slice() {
                    [] => match node_placed_between(&output.pin_type, &input.pin_type) {
                        Some(placement) => write!(formatter, "; {placement}"),
                        None => formatter.write_str("; no node type converts between them"),
                    },
                    [conversion] => write!(
                        formatter,
                        ". To fix: place a `{conversion}` node between them"
                    ),
                    conversions => write!(
                        formatter,
                        ". To fix: place one of {} between them",
                        in_backticks(conversions)
                    ),
                }
            }
        }
    }
}

/// The node or nodes a person places between an output of `output` and an input of `input` to
/// convert one into the other: an encoder or decoder only of a codec the encoded end states, or
/// of any when it states none, and between two unencoded types the node the family table names.
/// Between Image or Channels and Array the table's node states the pin types it converts, so it
/// is named only as a conversion that bridges, never by its family: an `nv12` Texture is not
/// sent to a `TextureToTensor` that does not take it.
fn node_placed_between(output: &PinType, input: &PinType) -> Option<String> {
    use CodecDirection::{Decode, Encode};
    use EncodedMedia::{Audio, Video};
    use PortFamily::{Array, Channels, Image};
    match (output, input) {
        (PinType::EncodedVideo { codecs }, PinType::Texture { .. }) => {
            codec_nodes_placed_between(Video, Decode, codecs)
        }
        (PinType::Texture { .. }, PinType::EncodedVideo { codecs }) => {
            codec_nodes_placed_between(Video, Encode, codecs)
        }
        (PinType::EncodedAudio { codecs }, PinType::Audio {}) => {
            codec_nodes_placed_between(Audio, Decode, codecs)
        }
        (PinType::Audio {}, PinType::EncodedAudio { codecs }) => {
            codec_nodes_placed_between(Audio, Encode, codecs)
        }
        (PinType::EncodedVideo { .. } | PinType::EncodedAudio { .. }, _)
        | (_, PinType::EncodedVideo { .. } | PinType::EncodedAudio { .. }) => None,
        _ => match (PortFamily::carrying(output)?, PortFamily::carrying(input)?) {
            (Image | Channels, Array) | (Array, Image | Channels) => None,
            (output_family, input_family) => {
                node_placed_between_families(output_family, input_family)
            }
        },
    }
}

impl std::error::Error for PinLinkRefusal {}

/// What a link does that a person may not expect. It never blocks the link.
#[derive(Debug, Clone, PartialEq)]
pub enum PinLinkWarning {
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
        member_name: String,
        /// The output member's meaning.
        output_meaning: String,
        /// The input member's meaning.
        input_meaning: String,
    },
    /// A Vector is read as a Color, its components as colour channels.
    VectorReadAsAColor,
    /// The output may send Menu options the input does not list.
    MenuOptionsTheInputDoesNotList {
        /// Each such option, as the output names it.
        option_names: Vec<String>,
    },
    /// The output states no pixel format, and the input takes only some.
    TextureOutputStatesNoFormat,
}

impl fmt::Display for PinLinkWarning {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MeaningsDiffer {
                output_meaning,
                input_meaning,
            } => write!(
                formatter,
                "the output carries `{output_meaning}` and the input takes `{input_meaning}`"
            ),
            Self::MemberMeaningsDiffer {
                member_name,
                output_meaning,
                input_meaning,
            } => write!(
                formatter,
                "member `{member_name}` of the output carries `{output_meaning}` and of the \
                 input takes `{input_meaning}`"
            ),
            Self::VectorReadAsAColor => formatter.write_str(
                "a Vector is read as a Color: x, y, z and w become red, green, blue and alpha",
            ),
            Self::MenuOptionsTheInputDoesNotList { option_names } => write!(
                formatter,
                "the output may send {}, which the input does not list",
                in_backticks(option_names)
            ),
            Self::TextureOutputStatesNoFormat => formatter.write_str(
                "the output states no pixel format, so it may send one the input cannot take",
            ),
        }
    }
}

impl PinLinkVerdict {
    /// The refusal a link made straight from one end to the other gets; `None` when it connects.
    pub fn refusal_of_the_link_made_as_it_is(&self) -> Option<&PinLinkRefusal> {
        match &self.outcome {
            PinLinkOutcome::Connects | PinLinkOutcome::ConnectsWithFreeConversion(_) => None,
            PinLinkOutcome::ConnectsThroughConvertNode(convert_node) => {
                Some(&convert_node.refusal_without_it)
            }
            PinLinkOutcome::Refused(refusal) => Some(refusal),
        }
    }
}

/// How a link from `output` to `input` connects, by their types and the stated `conversions`.
pub fn pin_link_verdict(
    output: PinLinkEnd<'_>,
    input: PinLinkInputEnd<'_>,
    conversions: &[PinConversion],
) -> PinLinkVerdict {
    if input
        .input_mode
        .is_some_and(|input_mode| !input_mode.takes_a_wire())
    {
        return PinLinkVerdict {
            outcome: PinLinkOutcome::Refused(PinLinkRefusal::input_takes_no_wire(
                input.end.node_name,
                input.end.port_name,
            )),
            warnings: Vec::new(),
        };
    }
    let input = input.end;
    let meanings_differ =
        differing_meanings(output.meaning, input.meaning).map(|(output_meaning, input_meaning)| {
            PinLinkWarning::MeaningsDiffer {
                output_meaning,
                input_meaning,
            }
        });
    if let Some(direct) = direct_link_between(output.pin_type, input.pin_type) {
        let mut warnings = direct.warnings;
        warnings.extend(meanings_differ);
        return PinLinkVerdict {
            outcome: match direct.free_conversion {
                None => PinLinkOutcome::Connects,
                Some(free_conversion) => {
                    PinLinkOutcome::ConnectsWithFreeConversion(free_conversion)
                }
            },
            warnings,
        };
    }
    let bridging: Vec<BridgingConversion<'_>> = conversions
        .iter()
        .filter_map(|conversion| {
            Some(BridgingConversion {
                conversion,
                into_it: direct_link_between(output.pin_type, &conversion.input_pin_type)?,
                out_of_it: direct_link_between(&conversion.output_pin_type, input.pin_type)?,
            })
        })
        .collect();
    let refusal = PinLinkRefusal::PinTypesDoNotConnect {
        output: output.into(),
        input: input.into(),
        conversions_that_bridge: bridging
            .iter()
            .map(|bridging| bridging.conversion.node_type.clone())
            .collect(),
    };
    let mut insertable = bridging
        .into_iter()
        .filter(|bridging| bridging.conversion.may_be_inserted());
    match (insertable.next(), insertable.next()) {
        (Some(only_one), None) => {
            let mut warnings = only_one.into_it.warnings;
            warnings.extend(only_one.out_of_it.warnings);
            warnings.extend(meanings_differ);
            PinLinkVerdict {
                outcome: PinLinkOutcome::ConnectsThroughConvertNode(ConvertNodeToInsert {
                    conversion: only_one.conversion.clone(),
                    refusal_without_it: refusal,
                }),
                warnings,
            }
        }
        _ => PinLinkVerdict {
            outcome: PinLinkOutcome::Refused(refusal),
            warnings: Vec::new(),
        },
    }
}

/// A conversion that bridges two ends, with the direct link into it and out of it.
struct BridgingConversion<'conversion> {
    conversion: &'conversion PinConversion,
    into_it: DirectPinLink,
    out_of_it: DirectPinLink,
}

/// A link a direct rule makes: as it is, or through a free conversion, with what it warns of.
struct DirectPinLink {
    free_conversion: Option<FreePinConversion>,
    warnings: Vec<PinLinkWarning>,
}

impl DirectPinLink {
    fn as_it_is() -> Self {
        Self {
            free_conversion: None,
            warnings: Vec::new(),
        }
    }

    fn warning(warning: PinLinkWarning) -> Self {
        Self {
            free_conversion: None,
            warnings: vec![warning],
        }
    }

    fn converting(free_conversion: FreePinConversion) -> Self {
        Self {
            free_conversion: Some(free_conversion),
            warnings: Vec::new(),
        }
    }
}

/// How an output of `output` connects to an input of `input` with nothing between them.
fn direct_link_between(output: &PinType, input: &PinType) -> Option<DirectPinLink> {
    use FreePinConversion::{
        BetweenNumbers, BetweenVectors, MenuOptionNameAsString, NumberSpreadOverColor,
        NumberSpreadOverVector, VectorIntoColor,
    };
    if !output.takes_a_wire() || !input.takes_a_wire() {
        return None;
    }
    match (output, input) {
        (
            PinType::Texture {
                pixel_formats: output_formats,
            },
            PinType::Texture {
                pixel_formats: input_formats,
            },
        ) => {
            if output_formats.is_empty() && !input_formats.is_empty() {
                Some(DirectPinLink::warning(
                    PinLinkWarning::TextureOutputStatesNoFormat,
                ))
            } else if every_one_is_taken(output_formats, input_formats) {
                Some(DirectPinLink::as_it_is())
            } else {
                None
            }
        }
        (
            PinType::EncodedVideo {
                codecs: output_codecs,
            },
            PinType::EncodedVideo {
                codecs: input_codecs,
            },
        )
        | (
            PinType::EncodedAudio {
                codecs: output_codecs,
            },
            PinType::EncodedAudio {
                codecs: input_codecs,
            },
        ) => states_only_what_the_input_takes(output_codecs, input_codecs),
        (
            PinType::Asset {
                media: output_media,
            },
            PinType::Asset { media: input_media },
        ) => states_only_what_the_input_takes(output_media, input_media),
        (
            PinType::Menu {
                option_names: output_options,
            },
            PinType::Menu {
                option_names: input_options,
            },
        ) => {
            let not_listed: Vec<String> = the_ones_not_held(output_options, input_options)
                .into_iter()
                .cloned()
                .collect();
            if not_listed.is_empty() {
                Some(DirectPinLink::as_it_is())
            } else {
                Some(DirectPinLink::warning(
                    PinLinkWarning::MenuOptionsTheInputDoesNotList {
                        option_names: not_listed,
                    },
                ))
            }
        }
        _ if the_same_but_for_member_meanings(output, input) => {
            let mut warnings = Vec::new();
            warn_of_member_meanings_that_differ(output, input, "", &mut warnings);
            Some(DirectPinLink {
                free_conversion: None,
                warnings,
            })
        }
        (output_number, input_number)
            if output_number.is_a_number() && input_number.is_a_number() =>
        {
            Some(DirectPinLink::converting(BetweenNumbers))
        }
        (PinType::Vector { .. }, PinType::Vector { .. }) => {
            Some(DirectPinLink::converting(BetweenVectors))
        }
        (PinType::Vector { .. }, PinType::Color {}) => Some(DirectPinLink {
            free_conversion: Some(VectorIntoColor),
            warnings: vec![PinLinkWarning::VectorReadAsAColor],
        }),
        (number, PinType::Vector { .. }) if number.is_a_number() => {
            Some(DirectPinLink::converting(NumberSpreadOverVector))
        }
        (number, PinType::Color {}) if number.is_a_number() => {
            Some(DirectPinLink::converting(NumberSpreadOverColor))
        }
        (PinType::Menu { .. }, PinType::String {}) => {
            Some(DirectPinLink::converting(MenuOptionNameAsString))
        }
        _ => None,
    }
}

/// The two meanings, when both ends state one and they differ.
fn differing_meanings(output: Option<&str>, input: Option<&str>) -> Option<(String, String)> {
    match (output, input) {
        (Some(output), Some(input)) if output != input => {
            Some((output.to_owned(), input.to_owned()))
        }
        _ => None,
    }
}

/// Push a warning for each bundle member, at any depth, whose two meanings differ.
fn warn_of_member_meanings_that_differ(
    output: &PinType,
    input: &PinType,
    path: &str,
    warnings: &mut Vec<PinLinkWarning>,
) {
    match (output, input) {
        (PinType::Bundle { members: output }, PinType::Bundle { members: input }) => {
            for (output_member, input_member) in
                members_paired_by_name(output, input).into_iter().flatten()
            {
                let member_name = format!("{path}{}", output_member.name);
                if let Some((output_meaning, input_meaning)) = differing_meanings(
                    output_member.meaning.as_deref(),
                    input_member.meaning.as_deref(),
                ) {
                    warnings.push(PinLinkWarning::MemberMeaningsDiffer {
                        member_name: member_name.clone(),
                        output_meaning,
                        input_meaning,
                    });
                }
                warn_of_member_meanings_that_differ(
                    &output_member.member_type,
                    &input_member.member_type,
                    &format!("{member_name}."),
                    warnings,
                );
            }
        }
        (
            PinType::ListOf {
                element_type: output,
            },
            PinType::ListOf {
                element_type: input,
            },
        ) => {
            warn_of_member_meanings_that_differ(output, input, path, warnings);
        }
        _ => {}
    }
}

/// Each member of `output` beside the member of `input` of its name, in `output`'s order; `None`
/// unless the two hold the same names, each held once by each.
fn members_paired_by_name<'members>(
    output: &'members [BundleMember],
    input: &'members [BundleMember],
) -> Option<Vec<(&'members BundleMember, &'members BundleMember)>> {
    if output.len() != input.len() {
        return None;
    }
    output
        .iter()
        .map(|output_member| {
            let held_once_by = |members: &'members [BundleMember]| {
                let mut named = members
                    .iter()
                    .filter(|member| member.name == output_member.name);
                match (named.next(), named.next()) {
                    (Some(member), None) => Some(member),
                    _ => None,
                }
            };
            held_once_by(output)?;
            Some((output_member, held_once_by(input)?))
        })
        .collect()
}

/// Whether `one` and `other` are one type, their bundle members matched by name and compared by
/// type only, in any order.
fn the_same_but_for_member_meanings(one: &PinType, other: &PinType) -> bool {
    match (one, other) {
        (PinType::Bundle { members: one }, PinType::Bundle { members: other }) => {
            members_paired_by_name(one, other).is_some_and(|pairs| {
                pairs.into_iter().all(|(one, other)| {
                    the_same_but_for_member_meanings(&one.member_type, &other.member_type)
                })
            })
        }
        (
            PinType::ListOf { element_type: one },
            PinType::ListOf {
                element_type: other,
            },
        ) => the_same_but_for_member_meanings(one, other),
        _ => one == other,
    }
}

/// Whether every one `output` states is one `input` takes, or `input` takes any by stating none.
fn every_one_is_taken<T: PartialEq>(output: &[T], input: &[T]) -> bool {
    the_ones_not_taken(output, input).is_empty()
}

/// A direct link when the input takes any, or the output states some and each is one it takes.
fn states_only_what_the_input_takes<T: PartialEq>(
    output: &[T],
    input: &[T],
) -> Option<DirectPinLink> {
    (input.is_empty() || (!output.is_empty() && every_one_is_taken(output, input)))
        .then(DirectPinLink::as_it_is)
}
