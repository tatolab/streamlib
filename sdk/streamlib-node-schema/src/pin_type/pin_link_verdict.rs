// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::fmt;

use super::{NodeInputMode, PinConversion, PinType};
use crate::NodeClassImportPath;

/// One end of a link judged by [`pin_link_verdict`]: the port, or one member of a bundle port,
/// with its resolved type.
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
    /// Whether an input takes a wire, a stored value, or both; `None` on an output, and on an
    /// input that takes a wire.
    pub input_mode: Option<NodeInputMode>,
}

/// What [`pin_link_verdict`] answers of a link: its outcome, and what a person should know of
/// it, none of which blocks it.
#[derive(Debug, Clone, PartialEq)]
pub struct PinLinkVerdict {
    /// Whether and how the link connects.
    pub outcome: PinLinkOutcome,
    /// What the link does that a person may not expect; never a reason to refuse it.
    pub warnings: Vec<PinLinkWarning>,
}

/// Whether and how a link between two typed pins connects.
#[derive(Debug, Clone, PartialEq)]
pub enum PinLinkOutcome {
    /// The input takes what the output carries as it is.
    Connects,
    /// The input reads what the output carries as its own type, at no cost.
    ConnectsWithFreeConversion(FreePinConversion),
    /// The two ends connect through a node of one built-in conversion placed between them.
    ConnectsThroughConvertNode(ConvertNodeToInsert),
    /// The two ends do not connect.
    Refused(PinLinkRefusal),
}

/// How an input reads what an output of another type carries. Each one holds for two types
/// whose messages are packed alike, so the reader takes its own type from the writer's message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FreePinConversion {
    /// Boolean, Integer and Float among themselves, or one of them in another size. A Float
    /// read as an Integer rounds half away from zero.
    BetweenNumbers,
    /// A Vector read as one of another size: components are dropped, or padded with 0.
    BetweenVectors,
    /// A Vector read as a Color: x, y, z and w as red, green, blue and alpha, a missing alpha
    /// padded with 1.
    VectorIntoColor,
    /// One number read as every component of a Vector.
    NumberSpreadOverVector,
    /// One number read as red, green and blue, with an alpha of 1.
    NumberSpreadOverColor,
    /// A Menu's option name read as a String.
    MenuOptionNameAsString,
}

/// The convert node a link needs between its two ends, and the refusal a link made with no such
/// node gets.
#[derive(Debug, Clone, PartialEq)]
pub struct ConvertNodeToInsert {
    /// The built-in conversion that bridges the two ends.
    pub conversion: PinConversion,
    /// The refusal of the link made straight from one end to the other, naming every
    /// conversion that bridges them.
    pub refusal_without_it: PinLinkRefusal,
}

/// Why a link is refused. Its words are the sentence a client shows, naming both ends.
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
        /// Each node type stating a conversion that bridges the two ends, to place between
        /// them.
        conversions_that_bridge: Vec<NodeClassImportPath>,
    },
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
                    [] => formatter.write_str("; no node type converts between them"),
                    [conversion] => write!(
                        formatter,
                        ". To fix: place a `{conversion}` node between them"
                    ),
                    conversions => {
                        let named: Vec<_> = conversions
                            .iter()
                            .map(|conversion| format!("`{conversion}`"))
                            .collect();
                        write!(
                            formatter,
                            ". To fix: place one of {} between them",
                            named.join(", ")
                        )
                    }
                }
            }
        }
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
    /// A Vector is read as a Color, its components as colour channels.
    VectorReadAsAColor,
    /// The output may send Menu options the input does not list.
    MenuOptionsTheInputDoesNotList {
        /// Each such option, as the output names it.
        option_names: Vec<String>,
    },
    /// The output states no pixel format, so it may send one the input cannot take.
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
            Self::VectorReadAsAColor => formatter.write_str(
                "a Vector is read as a Color: x, y, z and w become red, green, blue and alpha",
            ),
            Self::MenuOptionsTheInputDoesNotList { option_names } => write!(
                formatter,
                "the output may send {}, which the input does not list",
                option_names
                    .iter()
                    .map(|option_name| format!("`{option_name}`"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Self::TextureOutputStatesNoFormat => formatter.write_str(
                "the output states no pixel format, so it may send one the input cannot take",
            ),
        }
    }
}

impl PinLinkVerdict {
    /// The refusal a link made straight from one end to the other gets: none when it connects
    /// or converts for free; the refusal itself, or the one a link that needs a convert node
    /// gets without it.
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

/// How a link from `output` to `input`, two typed ends, connects, by their types alone and the
/// `conversions` node types state; this is the one rule a link between typed pins is judged by.
///
/// A link into a setting is refused. Two ends a direct rule joins connect, or connect with a
/// free conversion. Otherwise the link goes through a convert node when exactly one built-in
/// conversion bridges them, the output connecting to its input and its output to the input,
/// each by a direct rule; else it is refused, naming every conversion that bridges them.
/// With no conversions passed it answers the direct rules alone.
pub fn pin_link_verdict(
    output: PinLinkEnd<'_>,
    input: PinLinkEnd<'_>,
    conversions: &[PinConversion],
) -> PinLinkVerdict {
    if input
        .input_mode
        .is_some_and(|input_mode| !input_mode.takes_a_wire())
    {
        return PinLinkVerdict {
            outcome: PinLinkOutcome::Refused(PinLinkRefusal::InputTakesNoWire {
                node_name: input.node_name.to_owned(),
                input_name: input.port_name.to_owned(),
            }),
            warnings: Vec::new(),
        };
    }
    let meanings_differ = match (output.meaning, input.meaning) {
        (Some(output_meaning), Some(input_meaning)) if output_meaning != input_meaning => {
            Some(PinLinkWarning::MeaningsDiffer {
                output_meaning: output_meaning.to_owned(),
                input_meaning: input_meaning.to_owned(),
            })
        }
        _ => None,
    };
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
    let bridging: Vec<&PinConversion> = conversions
        .iter()
        .filter(|conversion| {
            direct_link_between(output.pin_type, &conversion.input_pin_type).is_some()
                && direct_link_between(&conversion.output_pin_type, input.pin_type).is_some()
        })
        .collect();
    let refusal = PinLinkRefusal::PinTypesDoNotConnect {
        output: output.into(),
        input: input.into(),
        conversions_that_bridge: bridging
            .iter()
            .map(|conversion| conversion.node_type.clone())
            .collect(),
    };
    let insertable: Vec<&PinConversion> = bridging
        .into_iter()
        .filter(|conversion| conversion.may_be_inserted())
        .collect();
    match insertable.as_slice() {
        [conversion] => PinLinkVerdict {
            outcome: PinLinkOutcome::ConnectsThroughConvertNode(ConvertNodeToInsert {
                conversion: (*conversion).clone(),
                refusal_without_it: refusal,
            }),
            warnings: meanings_differ.into_iter().collect(),
        },
        _ => PinLinkVerdict {
            outcome: PinLinkOutcome::Refused(refusal),
            warnings: Vec::new(),
        },
    }
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

/// How an output of `output` connects to an input of `input` with nothing between them; `None`
/// when no direct rule joins them.
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
            if output_formats.is_empty() {
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
            let not_listed: Vec<String> = output_options
                .iter()
                .filter(|option_name| !input_options.contains(option_name))
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
        _ if output == input => Some(DirectPinLink::as_it_is()),
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

/// Whether every one `output` states is one `input` takes, or `input` takes any by stating none.
fn every_one_is_taken<T: PartialEq>(output: &[T], input: &[T]) -> bool {
    input.is_empty() || output.iter().all(|stated| input.contains(stated))
}

/// A link that holds when the input takes any, or the output states some and each is one the
/// input takes; an output stating none may send what the input does not take.
fn states_only_what_the_input_takes<T: PartialEq>(
    output: &[T],
    input: &[T],
) -> Option<DirectPinLink> {
    (input.is_empty() || (!output.is_empty() && every_one_is_taken(output, input)))
        .then(DirectPinLink::as_it_is)
}

impl PinType {
    /// Whether a port of this type may be the end of a link: every type but a setting-only one.
    pub fn takes_a_wire(&self) -> bool {
        !matches!(
            self,
            Self::NodeReference { .. } | Self::PinTypeChoice { .. }
        )
    }

    /// Whether this is a Boolean, an Integer or a Float, each a message of one number.
    fn is_a_number(&self) -> bool {
        matches!(
            self,
            Self::Boolean {} | Self::Integer { .. } | Self::Float { .. }
        )
    }
}
