// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::fmt;

use super::{BundleMember, NodeInputMode, PinConversion, PinConversionOutputPicked, PinType};
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
    ConnectsThroughConvertNode(Box<ConvertNodeToInsert>),
    /// The two ends do not connect.
    Refused(Box<PinLinkRefusal>),
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

/// The convert node a link needs, the settings it holds, and the refusal of the link made
/// without it.
#[derive(Debug, Clone, PartialEq)]
pub struct ConvertNodeToInsert {
    /// The built-in conversion that bridges the two ends.
    pub conversion: PinConversion,
    /// The settings the conversion picks for the input it feeds, each by its input's name, which
    /// the node holds as its input values; empty when its output follows no setting.
    pub input_values: serde_json::Map<String, serde_json::Value>,
    /// The type the node's output gives under those settings.
    pub output_pin_type: PinType,
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
        /// Each conversion that bridges the two ends, none of which `connect` places.
        conversions_that_bridge: Vec<PinConversion>,
    },
    /// The output carries a bundle or a List holding a Texture, Audio or an encoded unit, which
    /// travels as one Bundle frame, and the input states no pin type, so it reads each message
    /// as it arrives and cannot take the frame apart.
    BundleFrameIntoAnUntypedInput {
        /// The output end.
        output: PinLinkEndNamed,
        /// The node of the input.
        input_node_name: String,
        /// The input.
        input_name: String,
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

/// `phrases`, each of which may hold a comma, as a sentence lists them: `a`, `a; and b`,
/// `a; b; and c`.
fn in_words_listed(phrases: &[String]) -> String {
    match phrases {
        [] => String::new(),
        [only] => only.clone(),
        [listed @ .., last] => format!("{}; and {last}", listed.join("; ")),
    }
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
                let members_not_held =
                    members_the_output_does_not_hold(&output.pin_type, &input.pin_type, "");
                if !members_not_held.is_empty() {
                    write!(
                        formatter,
                        ": the input asks for {}",
                        in_words_listed(&members_not_held)
                    )?;
                }
                match conversions_that_bridge.as_slice() {
                    [] if !members_not_held.is_empty() => Ok(()),
                    [] => match node_placed_between(&output.pin_type, &input.pin_type) {
                        Some(placement) => write!(formatter, "; {placement}"),
                        None => formatter.write_str("; no node type converts between them"),
                    },
                    [conversion] => write!(
                        formatter,
                        ". To fix: place a `{}` node between them",
                        conversion.node_type
                    ),
                    conversions => write!(
                        formatter,
                        ". To fix: place one of {} between them",
                        in_backticks(conversions.iter().map(|conversion| &conversion.node_type))
                    ),
                }
            }
            Self::BundleFrameIntoAnUntypedInput {
                output,
                input_node_name,
                input_name,
            } => write!(
                formatter,
                "cannot link {output} to `{input_node_name}.{input_name}`, which states no pin \
                 type and reads each message as it arrives: {}, so the output's messages travel \
                 as Bundle frames, which only an input typed as the bundle takes apart. To fix: \
                 link it to an input of a bundle type, or send each heavy member on a port of \
                 its own",
                output
                    .pin_type
                    .its_heavy_members_in_words()
                    .unwrap_or_else(|| "it holds a heavy member".to_owned())
            ),
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
    /// A node type's port finds its type from a link into its node or from a setting, so it is
    /// judged as taking the other end's type before a node of the type is placed.
    TypeFoundOnlyOncePlaced,
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
            Self::TypeFoundOnlyOncePlaced => formatter.write_str(
                "the node type's port finds its type from a link into its node or from a setting, \
                 so it is judged as taking the other end's type until a node of the type is \
                 placed",
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
            outcome: PinLinkOutcome::Refused(Box::new(PinLinkRefusal::input_takes_no_wire(
                input.end.node_name,
                input.end.port_name,
            ))),
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
            let picked = conversion.output_picked_for(input.pin_type)?;
            Some(BridgingConversion {
                conversion,
                into_it: direct_link_between(output.pin_type, &conversion.input_pin_type)?,
                out_of_it: direct_link_between(&picked.output_pin_type, input.pin_type)?,
                picked,
            })
        })
        .collect();
    let refusal = PinLinkRefusal::PinTypesDoNotConnect {
        output: output.into(),
        input: input.into(),
        conversions_that_bridge: bridging
            .iter()
            .map(|bridging| bridging.conversion.clone())
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
                outcome: PinLinkOutcome::ConnectsThroughConvertNode(Box::new(
                    ConvertNodeToInsert {
                        conversion: only_one.conversion.clone(),
                        input_values: only_one.picked.input_values,
                        output_pin_type: only_one.picked.output_pin_type,
                        refusal_without_it: refusal,
                    },
                )),
                warnings,
            }
        }
        _ => PinLinkVerdict {
            outcome: PinLinkOutcome::Refused(Box::new(refusal)),
            warnings: Vec::new(),
        },
    }
}

/// A conversion that bridges two ends, with the direct link into it and out of it, and what it
/// picks to feed the input.
struct BridgingConversion<'conversion> {
    conversion: &'conversion PinConversion,
    into_it: DirectPinLink,
    out_of_it: DirectPinLink,
    picked: PinConversionOutputPicked,
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
        _ if holds_what_the_input_asks_for(output, input) => {
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
            for (output_member, input_member) in members_the_input_asks_for_paired(output, input)
                .into_iter()
                .flatten()
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

/// Each member `input` asks for beside the member of `output` of its name, in `input`'s order;
/// `None` unless each name `input` asks for is named once by `input` and once by `output`.
fn members_the_input_asks_for_paired<'members>(
    output: &'members [BundleMember],
    input: &'members [BundleMember],
) -> Option<Vec<(&'members BundleMember, &'members BundleMember)>> {
    input
        .iter()
        .map(|input_member| {
            let named_once_by = |members: &'members [BundleMember]| {
                let mut named = members
                    .iter()
                    .filter(|member| member.name == input_member.name);
                match (named.next(), named.next()) {
                    (Some(member), None) => Some(member),
                    _ => None,
                }
            };
            named_once_by(input)?;
            Some((named_once_by(output)?, input_member))
        })
        .collect()
}

/// Whether an output of `output` holds what an input of `input` asks for, at any depth, as it
/// is: the same type, but that a bundle output may hold members the input does not ask for, and
/// that a member's meaning is not compared. Each member the input asks for is matched by name
/// and compared by type.
fn holds_what_the_input_asks_for(output: &PinType, input: &PinType) -> bool {
    match (output, input) {
        (PinType::Bundle { members: output }, PinType::Bundle { members: input }) => {
            members_the_input_asks_for_paired(output, input).is_some_and(|pairs| {
                pairs.into_iter().all(|(output, input)| {
                    holds_what_the_input_asks_for(&output.member_type, &input.member_type)
                })
            })
        }
        (
            PinType::ListOf {
                element_type: output,
            },
            PinType::ListOf {
                element_type: input,
            },
        ) => holds_what_the_input_asks_for(output, input),
        (
            PinType::Connection {
                connection_type: output,
                ..
            },
            PinType::Connection {
                connection_type: input,
                ..
            },
        ) => output == input,
        _ => output == input,
    }
}

/// Why a bundle output of `output` does not hold what a bundle input of `input` asks for, member
/// by member: each member the input asks for that the output does not hold, or holds under
/// another type. Empty for two types that are no bundles, nor Lists of them.
fn members_the_output_does_not_hold(output: &PinType, input: &PinType, path: &str) -> Vec<String> {
    match (output, input) {
        (PinType::Bundle { members: output }, PinType::Bundle { members: input }) => input
            .iter()
            .enumerate()
            .flat_map(|(place, input_member)| {
                let member_name = format!("{path}{}", input_member.name);
                let named_earlier = input[..place]
                    .iter()
                    .any(|member| member.name == input_member.name);
                let named_later = input[place + 1..]
                    .iter()
                    .any(|member| member.name == input_member.name);
                let mut output_named = output
                    .iter()
                    .filter(|member| member.name == input_member.name);
                match (output_named.next(), output_named.next()) {
                    _ if named_earlier => Vec::new(),
                    _ if named_later => vec![format!(
                        "member `{member_name}`, which the input names more than once"
                    )],
                    (None, _) => vec![format!(
                        "member `{member_name}`, which the output does not hold"
                    )],
                    (Some(_), Some(_)) => vec![format!(
                        "member `{member_name}`, which the output names more than once"
                    )],
                    (Some(output_member), None) => {
                        let inner = members_the_output_does_not_hold(
                            &output_member.member_type,
                            &input_member.member_type,
                            &format!("{member_name}."),
                        );
                        if !inner.is_empty() {
                            inner
                        } else if holds_what_the_input_asks_for(
                            &output_member.member_type,
                            &input_member.member_type,
                        ) {
                            Vec::new()
                        } else {
                            vec![format!(
                                "member `{member_name}` as {}, which the output holds as {}",
                                input_member.member_type, output_member.member_type
                            )]
                        }
                    }
                }
            })
            .collect(),
        (
            PinType::ListOf {
                element_type: output,
            },
            PinType::ListOf {
                element_type: input,
            },
        ) => members_the_output_does_not_hold(output, input, path),
        _ => Vec::new(),
    }
}

/// Whether every one `output` states is one `input` takes, or `input` takes any by stating none.
fn every_one_is_taken<T: PartialEq>(output: &[T], input: &[T]) -> bool {
    the_ones_not_taken(output, input).next().is_none()
}

/// A direct link when the input takes any, or the output states some and each is one it takes.
fn states_only_what_the_input_takes<T: PartialEq>(
    output: &[T],
    input: &[T],
) -> Option<DirectPinLink> {
    (input.is_empty() || (!output.is_empty() && every_one_is_taken(output, input)))
        .then(DirectPinLink::as_it_is)
}
