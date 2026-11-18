// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The five port families a port declares, the keys each takes, and the
//! checks `connect` makes between an output and an input of one family.

use std::collections::BTreeSet;
use std::fmt;

use serde::{Deserialize, Serialize};

use crate::pin_type::TexturePixelFormat;

/// The memory layout a port carries; a link joins two ports of one family.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum PortFamily {
    Image,
    Array,
    Channels,
    Packets,
    Records,
}

impl PortFamily {
    /// Every family, in declaration order.
    pub const ALL: [PortFamily; 5] = [
        PortFamily::Image,
        PortFamily::Array,
        PortFamily::Channels,
        PortFamily::Packets,
        PortFamily::Records,
    ];

    /// The name a declaration spells this family by.
    pub fn declaration_name(self) -> &'static str {
        match self {
            PortFamily::Image => "Image",
            PortFamily::Array => "Array",
            PortFamily::Channels => "Channels",
            PortFamily::Packets => "Packets",
            PortFamily::Records => "Records",
        }
    }

    /// The name a graph, a config and the wire spell this family by.
    pub fn wire_name(self) -> &'static str {
        match self {
            PortFamily::Image => "image",
            PortFamily::Array => "array",
            PortFamily::Channels => "channels",
            PortFamily::Packets => "packets",
            PortFamily::Records => "records",
        }
    }

    /// The family a graph, a config or the wire spells `wire_name`; `None` for a name no
    /// family has.
    pub fn of_wire_name(wire_name: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|family| family.wire_name() == wire_name)
    }

    /// What a port of this family carries: CommonMark, served as written.
    pub fn description(self) -> &'static str {
        match self {
            PortFamily::Image => "A GPU surface with its size, pixel format and colour.",
            PortFamily::Array => "Named tensors, on the GPU or the CPU.",
            PortFamily::Channels => "Named sample channels at one rate.",
            PortFamily::Packets => "Encoded units of one codec.",
            PortFamily::Records => "One record per message, a value under each declared name.",
        }
    }

    /// Whether a port of this family states names: channel names, attribute
    /// names or record keys.
    pub fn has_names(self) -> bool {
        matches!(
            self,
            PortFamily::Array | PortFamily::Channels | PortFamily::Records
        )
    }
}

impl fmt::Display for PortFamily {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.declaration_name())
    }
}

/// The names an output port states: written on the class, or read from one
/// key of its node's config when `connect` resolves them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum OutputPortDeclaredNames {
    Stated(Vec<String>),
    FromConfig { config_key: String },
}

/// The sample rate a Channels output states, written on the class or read
/// from its node's config.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum ChannelsOutputPortSampleRate {
    Stated(u32),
    FromConfig { config_key: String },
}

/// The frame rate an Image output states, written on the class or read from
/// its node's config.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum ImageOutputPortFrameRate {
    Stated(crate::ImageStreamFrameRate),
    FromConfig { config_key: String },
}

/// The sample type of a Channels output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum ChannelsOutputPortSampleType {
    F32,
    F64,
}

/// Where an Array output's attributes live.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum ArrayOutputPortResidency {
    Gpu,
    Cpu,
}

/// A port's family and the keys its family takes.
///
/// An output states `names`, `codec`, `rate`, `frame_rate`, `dtype`, `formats` and
/// `residency` as its family allows; an input states `requires`,
/// `requires_codec` and `formats`. [`Self::refusal_of_its_keys`] names a key
/// the family or the direction does not take.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
pub struct PortFamilyDeclaration {
    pub family: PortFamily,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub names: Option<OutputPortDeclaredNames>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub requires: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub codec: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub requires_codec: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rate: Option<ChannelsOutputPortSampleRate>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frame_rate: Option<ImageOutputPortFrameRate>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dtype: Option<ChannelsOutputPortSampleType>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub formats: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub residency: Option<ArrayOutputPortResidency>,
    /// The families an input takes a link from besides [`Self::family`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub also_takes_families: Vec<PortFamily>,
    /// The config key an output reads its family from when `connect` resolves
    /// it; [`Self::family`] while the key is unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub family_from_config_key: Option<String>,
    /// The config key an output reads its codec from when `connect` resolves
    /// it; [`Self::codec`] while the key is unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub codec_from_config_key: Option<String>,
    /// The input port of the same node whose upstream family, names and
    /// codec an output carries on, so `connect` resolves it through that
    /// input's link.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub family_follows_input_port: Option<String>,
}

/// Which end of a link a port declaration sits at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PortFamilyDeclarationDirection {
    Input,
    Output,
}

impl PortFamilyDeclaration {
    /// A declaration of `family` with none of its keys stated.
    pub fn of_family(family: PortFamily) -> Self {
        Self {
            family,
            names: None,
            requires: Vec::new(),
            codec: None,
            requires_codec: Vec::new(),
            rate: None,
            frame_rate: None,
            dtype: None,
            formats: Vec::new(),
            residency: None,
            also_takes_families: Vec::new(),
            family_from_config_key: None,
            codec_from_config_key: None,
            family_follows_input_port: None,
        }
    }

    /// Every family this input takes a link from.
    pub fn families_an_input_takes(&self) -> Vec<PortFamily> {
        let mut families = vec![self.family];
        for family in &self.also_takes_families {
            if !families.contains(family) {
                families.push(*family);
            }
        }
        families
    }

    /// The first key this declaration states that its family or its
    /// direction does not take, as a refusal naming the port and the key.
    pub fn refusal_of_its_keys(
        &self,
        port_name: &str,
        direction: PortFamilyDeclarationDirection,
    ) -> Option<String> {
        let family = self.family;
        let is_an_output = direction == PortFamilyDeclarationDirection::Output;
        let output_family_may_be_packets =
            family == PortFamily::Packets || self.family_from_config_key.is_some();
        let output_family_may_have_names =
            family.has_names() || self.family_from_config_key.is_some();
        let families_an_input_takes = self.families_an_input_takes();
        let input_takes_a_family_with_names = families_an_input_takes
            .iter()
            .any(|family| family.has_names());
        let input_takes_packets = families_an_input_takes.contains(&PortFamily::Packets);
        let stated_keys: [(&str, bool, bool); 14] = [
            (
                "names",
                self.names.is_some(),
                is_an_output && output_family_may_have_names,
            ),
            (
                "requires",
                !self.requires.is_empty(),
                direction == PortFamilyDeclarationDirection::Input
                    && input_takes_a_family_with_names,
            ),
            (
                "codec",
                self.codec.is_some(),
                direction == PortFamilyDeclarationDirection::Output
                    && family == PortFamily::Packets,
            ),
            (
                "requires_codec",
                !self.requires_codec.is_empty(),
                direction == PortFamilyDeclarationDirection::Input && input_takes_packets,
            ),
            (
                "rate",
                self.rate.is_some(),
                direction == PortFamilyDeclarationDirection::Output
                    && family == PortFamily::Channels,
            ),
            (
                "frame_rate",
                self.frame_rate.is_some(),
                direction == PortFamilyDeclarationDirection::Output && family == PortFamily::Image,
            ),
            (
                "dtype",
                self.dtype.is_some(),
                direction == PortFamilyDeclarationDirection::Output
                    && family == PortFamily::Channels,
            ),
            (
                "formats",
                !self.formats.is_empty(),
                family == PortFamily::Image,
            ),
            (
                "residency",
                self.residency.is_some(),
                direction == PortFamilyDeclarationDirection::Output && family == PortFamily::Array,
            ),
            (
                "also_takes_families",
                !self.also_takes_families.is_empty(),
                direction == PortFamilyDeclarationDirection::Input,
            ),
            (
                "family_from_config_key",
                self.family_from_config_key.is_some(),
                is_an_output,
            ),
            (
                "codec_from_config_key",
                self.codec_from_config_key.is_some(),
                is_an_output && output_family_may_be_packets,
            ),
            (
                "family_follows_input_port",
                self.family_follows_input_port.is_some(),
                is_an_output,
            ),
            ("family", false, true),
        ];
        let direction_word = match direction {
            PortFamilyDeclarationDirection::Input => "input",
            PortFamilyDeclarationDirection::Output => "output",
        };
        stated_keys
            .iter()
            .find(|(_, stated, taken)| *stated && !*taken)
            .map(|(key, _, _)| {
                format!(
                    "{direction_word} port {port_name:?} of family {family} states `{key}=`, \
                     which a {family} {direction_word} does not take"
                )
            })
            .or_else(|| {
                self.formats
                    .iter()
                    .find(|format| !IMAGE_TEXTURE_FORMAT_WIRE_NAMES.contains(&format.as_str()))
                    .map(|format| {
                        format!(
                            "{direction_word} port {port_name:?} states format {format:?}; \
                             `formats=` takes the engine's texture format names: {}",
                            IMAGE_TEXTURE_FORMAT_WIRE_NAMES.join(", ")
                        )
                    })
            })
    }
}

/// The texture format wire names an Image port's `formats=` takes: every
/// `TextureFormat` the engine's RHI defines, spelled as the escalate wire spells it.
pub const IMAGE_TEXTURE_FORMAT_WIRE_NAMES: [&str; TexturePixelFormat::EVERY_FORMAT.len()] = {
    let mut wire_names = [""; TexturePixelFormat::EVERY_FORMAT.len()];
    let mut position = 0;
    while position < wire_names.len() {
        wire_names[position] = TexturePixelFormat::EVERY_FORMAT[position].wire_name();
        position += 1;
    }
    wire_names
};

/// One end of a link, as a refusal names it.
#[derive(Debug, Clone, Copy)]
pub struct PortFamilyLinkEndpoint<'a> {
    pub node_name: &'a str,
    pub port_name: &'a str,
    pub declaration: &'a PortFamilyDeclaration,
}

impl fmt::Display for PortFamilyLinkEndpoint<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}.{}", self.node_name, self.port_name)
    }
}

/// The names an output's declaration resolves to against its node's config,
/// or why they cannot be resolved; `None` when the output states no names.
pub fn output_port_names_resolved_against_config(
    names: Option<&OutputPortDeclaredNames>,
    node_config: Option<&serde_json::Value>,
) -> Option<Result<Vec<String>, String>> {
    match names? {
        OutputPortDeclaredNames::Stated(names) => Some(Ok(names.clone())),
        OutputPortDeclaredNames::FromConfig { config_key } => {
            let value = node_config.and_then(|config| config.get(config_key));
            Some(match value {
                Some(serde_json::Value::Array(entries)) => entries
                    .iter()
                    .map(|entry| {
                        entry.as_str().map(str::to_owned).ok_or_else(|| {
                            format!(
                                "config key {config_key:?} must be a list of strings, and holds \
                                 {entry}"
                            )
                        })
                    })
                    .collect(),
                Some(other) => Err(format!(
                    "config key {config_key:?} must be a list of strings, and holds {other}"
                )),
                None => Err(format!(
                    "the names come from config key {config_key:?}, which the node's config \
                     does not set"
                )),
            })
        }
    }
}

/// The family an output's declaration resolves to against its node's config,
/// or why it cannot be resolved.
pub fn output_port_family_resolved_against_config(
    declaration: &PortFamilyDeclaration,
    node_config: Option<&serde_json::Value>,
) -> Result<PortFamily, String> {
    let Some(config_key) = declaration.family_from_config_key.as_deref() else {
        return Ok(declaration.family);
    };
    match node_config.and_then(|config| config.get(config_key)) {
        None | Some(serde_json::Value::Null) => Ok(declaration.family),
        Some(serde_json::Value::String(wire_name)) => PortFamily::of_wire_name(wire_name)
            .ok_or_else(|| {
                format!(
                    "config key {config_key:?} names family {wire_name:?}, and a family is one \
                     of {}",
                    PortFamily::ALL.map(|family| family.wire_name()).join(", ")
                )
            }),
        Some(other) => Err(format!(
            "config key {config_key:?} names the output's family as a string, and holds {other}"
        )),
    }
}

/// The frame rate an Image output's declaration resolves to against its node's
/// config — a whole number of frames per second or a `{numerator,
/// denominator}` map — or why it cannot be resolved; `None` when it states none
/// or its config key is unset.
pub fn output_port_frame_rate_resolved_against_config(
    declaration: &PortFamilyDeclaration,
    node_config: Option<&serde_json::Value>,
) -> Result<Option<crate::ImageStreamFrameRate>, String> {
    let config_key = match declaration.frame_rate.as_ref() {
        None => return Ok(None),
        Some(ImageOutputPortFrameRate::Stated(frame_rate)) => return Ok(Some(*frame_rate)),
        Some(ImageOutputPortFrameRate::FromConfig { config_key }) => config_key,
    };
    let refusal = |held: &serde_json::Value| {
        format!(
            "config key {config_key:?} states the frame rate as a positive whole number or a \
             {{numerator, denominator}} map, and holds {held}"
        )
    };
    match node_config.and_then(|config| config.get(config_key)) {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(held @ serde_json::Value::Number(number)) => number
            .as_u64()
            .and_then(|whole| u32::try_from(whole).ok())
            .filter(|whole| *whole > 0)
            .map(|numerator| {
                Some(crate::ImageStreamFrameRate {
                    numerator,
                    denominator: 1,
                })
            })
            .ok_or_else(|| refusal(held)),
        Some(held) => serde_json::from_value::<crate::ImageStreamFrameRate>(held.clone())
            .ok()
            .filter(|frame_rate| frame_rate.numerator > 0 && frame_rate.denominator > 0)
            .map(Some)
            .ok_or_else(|| refusal(held)),
    }
}

/// The codec an output's declaration resolves to against its node's config,
/// or why it cannot be resolved; `None` when it states none.
pub fn output_port_codec_resolved_against_config(
    declaration: &PortFamilyDeclaration,
    node_config: Option<&serde_json::Value>,
) -> Result<Option<String>, String> {
    let Some(config_key) = declaration.codec_from_config_key.as_deref() else {
        return Ok(declaration.codec.clone());
    };
    match node_config.and_then(|config| config.get(config_key)) {
        None | Some(serde_json::Value::Null) => Ok(declaration.codec.clone()),
        Some(serde_json::Value::String(codec)) if !codec.is_empty() => Ok(Some(codec.clone())),
        Some(other) => Err(format!(
            "config key {config_key:?} names the output's codec as a non-empty string, and \
             holds {other}"
        )),
    }
}

/// What the packets of a codec encode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EncodedMedia {
    Video,
    Audio,
}

/// A codec, what its packets encode, and the nodes that encode into it and decode out of it.
pub(crate) struct CodecNodes {
    pub(crate) codec: &'static str,
    pub(crate) media: EncodedMedia,
    pub(crate) encoder: &'static str,
    pub(crate) decoder: &'static str,
}

/// Each codec a node of the runtime's encodes into and decodes out of.
pub(crate) const CODEC_NODES: [CodecNodes; 3] = [
    CodecNodes {
        codec: "h264",
        media: EncodedMedia::Video,
        encoder: "H264Encoder",
        decoder: "H264Decoder",
    },
    CodecNodes {
        codec: "h265",
        media: EncodedMedia::Video,
        encoder: "H265Encoder",
        decoder: "H265Decoder",
    },
    CodecNodes {
        codec: "opus",
        media: EncodedMedia::Audio,
        encoder: "OpusEncoder",
        decoder: "OpusDecoder",
    },
];

/// Whether a person converts toward the packets of a codec, or away from them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CodecDirection {
    Encode,
    Decode,
}

/// The words placing the encoder or decoder, as `direction` says, of each codec of `media`
/// that `stated` lists, or of each when it lists none; `None` when no codec node fits. One
/// decoder takes one codec, so packets stating several are told to state one a decoder takes
/// first.
pub(crate) fn codec_nodes_placed_between(
    media: EncodedMedia,
    direction: CodecDirection,
    stated: &[String],
) -> Option<String> {
    let fitting: Vec<&CodecNodes> = CODEC_NODES
        .iter()
        .filter(|nodes| nodes.media == media)
        .filter(|nodes| {
            stated.is_empty()
                || stated
                    .iter()
                    .any(|codec| codec.eq_ignore_ascii_case(nodes.codec))
        })
        .collect();
    let node_of = |nodes: &CodecNodes| match direction {
        CodecDirection::Encode => nodes.encoder,
        CodecDirection::Decode => nodes.decoder,
    };
    let one_decoder_takes_every_codec_stated =
        direction == CodecDirection::Encode || stated.len() <= 1;
    match fitting.as_slice() {
        [] => None,
        [one] if one_decoder_takes_every_codec_stated => {
            Some(format!("place an {} node between them", node_of(one)))
        }
        several if one_decoder_takes_every_codec_stated => Some(format!(
            "place one of {} between them",
            several
                .iter()
                .map(|nodes| node_of(nodes))
                .collect::<Vec<_>>()
                .join(", ")
        )),
        [one] => Some(format!(
            "no one decoder takes every codec the output states; narrow the output to {}, then \
             place an {} node between them",
            one.codec,
            node_of(one)
        )),
        several => Some(format!(
            "no one decoder takes every codec the output states; narrow the output to one of \
             {}, then place its decoder between them",
            several
                .iter()
                .map(|nodes| nodes.codec)
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

/// The node a person places between an output of family `from` and an input of family `to`
/// to convert one to the other; `None` when no one node does.
pub(crate) fn node_placed_between_families(from: PortFamily, to: PortFamily) -> Option<String> {
    use PortFamily::*;
    let codec_nodes = |media, direction| codec_nodes_placed_between(media, direction, &[]);
    let one_node = |words: &str| Some(words.to_owned());
    match (from, to) {
        (Image, Array) => one_node("place an ImageToArray node between them"),
        (Array, Image) => one_node("place an ArrayToImage node between them"),
        (Channels, Array) => one_node("place a ChannelsToArray node between them"),
        (Array, Channels) => one_node("place an ArrayToChannels node between them"),
        (Channels, Records) => one_node("place a ChannelsToRecords node between them"),
        (Records, Channels) => one_node("place a RecordsToChannels node between them"),
        (Image, Packets) => codec_nodes(EncodedMedia::Video, CodecDirection::Encode),
        (Packets, Image) => codec_nodes(EncodedMedia::Video, CodecDirection::Decode),
        (Channels, Packets) => codec_nodes(EncodedMedia::Audio, CodecDirection::Encode),
        (Packets, Channels) => codec_nodes(EncodedMedia::Audio, CodecDirection::Decode),
        _ => None,
    }
}

/// The node a person places between `from` and `to` to convert one family to
/// the other.
fn conversion_between_families(from: PortFamily, to: PortFamily) -> String {
    node_placed_between_families(from, to).unwrap_or_else(|| {
        "no one node converts between them; chain the conversion nodes that do".to_owned()
    })
}

/// Why `connect` refuses a link from `output` to `input`, or `None` when it
/// takes it.
///
/// The output's family, resolved against its node's config, must be one the
/// input takes; then the input's `requires=` is checked against the output's
/// resolved names, `requires_codec=` against its resolved codec, and the two
/// `formats=` sets, which must overlap.
pub fn refusal_of_a_link_between_port_families(
    output: PortFamilyLinkEndpoint<'_>,
    output_node_config: Option<&serde_json::Value>,
    input: PortFamilyLinkEndpoint<'_>,
) -> Option<String> {
    let output_declaration = output.declaration;
    let input_declaration = input.declaration;
    let output_family =
        match output_port_family_resolved_against_config(output_declaration, output_node_config) {
            Ok(family) => family,
            Err(unresolved) => {
                return Some(format!(
                    "cannot link {output} to {input}: {output}'s family cannot be resolved: \
                     {unresolved}"
                ));
            }
        };
    let input_families = input_declaration.families_an_input_takes();
    if !input_families.contains(&output_family) {
        let input_family = input_declaration.family;
        let input_families_named = input_families
            .iter()
            .map(|family| family.declaration_name())
            .collect::<Vec<_>>()
            .join(" or ");
        return Some(format!(
            "cannot link {output} ({output_family}) to {input} ({input_families_named}): a link \
             joins two ports of one family; {}",
            conversion_between_families(output_family, input_family)
        ));
    }
    if let Some(refusal) = refusal_of_the_names_and_codec_a_link_carries(
        output,
        output_family,
        output_node_config,
        input,
    ) {
        return Some(refusal);
    }
    if output_family == PortFamily::Image
        && !output_declaration.formats.is_empty()
        && !input_declaration.formats.is_empty()
    {
        let output_formats: BTreeSet<&String> = output_declaration.formats.iter().collect();
        let formats_overlap = input_declaration
            .formats
            .iter()
            .any(|format| output_formats.contains(format));
        if !formats_overlap {
            return Some(format!(
                "cannot link {output} to {input}: {output} writes formats {:?}, and {input} \
                 reads only {:?}",
                output_declaration.formats, input_declaration.formats
            ));
        }
    }
    None
}

/// Why an output of `output_family` cannot feed `input` by the names and the codec it resolves
/// against its node's config; `None` when the input requires none it lacks.
pub fn refusal_of_the_names_and_codec_a_link_carries(
    output: PortFamilyLinkEndpoint<'_>,
    output_family: PortFamily,
    output_node_config: Option<&serde_json::Value>,
    input: PortFamilyLinkEndpoint<'_>,
) -> Option<String> {
    let output_declaration = output.declaration;
    let input_declaration = input.declaration;
    if output_family.has_names() && !input_declaration.requires.is_empty() {
        let resolved_names = output_port_names_resolved_against_config(
            output_declaration.names.as_ref(),
            output_node_config,
        );
        let output_names = match resolved_names {
            None => {
                return Some(format!(
                    "cannot link {output} to {input}: {input} requires {:?}, and {output} \
                     states no names",
                    input_declaration.requires
                ));
            }
            Some(Err(unresolved)) => {
                return Some(format!(
                    "cannot link {output} to {input}: {input} requires {:?}, and {output}'s \
                     names cannot be resolved: {unresolved}",
                    input_declaration.requires
                ));
            }
            Some(Ok(names)) => names,
        };
        if let Some(refusal) =
            refusal_of_required_names(output, &output_names, input, &input_declaration.requires)
        {
            return Some(refusal);
        }
    }
    if output_family == PortFamily::Packets && !input_declaration.requires_codec.is_empty() {
        let output_codec =
            match output_port_codec_resolved_against_config(output_declaration, output_node_config)
            {
                Ok(codec) => codec,
                Err(unresolved) => {
                    return Some(format!(
                        "cannot link {output} to {input}: {output}'s codec cannot be resolved: \
                     {unresolved}"
                    ));
                }
            };
        let codec_matches = output_codec
            .as_ref()
            .is_some_and(|codec| input_declaration.requires_codec.contains(codec));
        if !codec_matches {
            return Some(format!(
                "cannot link {output} to {input}: {input} requires a codec in {:?}, and {output} \
                 states {}",
                input_declaration.requires_codec,
                output_codec
                    .as_deref()
                    .map_or_else(|| "no codec".to_owned(), |codec| format!("{codec:?}"))
            ));
        }
    }
    None
}

/// Why an output whose names resolve to `output_names` cannot feed an input
/// requiring `required_names`, naming each missing one.
pub fn refusal_of_required_names(
    output: PortFamilyLinkEndpoint<'_>,
    output_names: &[String],
    input: PortFamilyLinkEndpoint<'_>,
    required_names: &[String],
) -> Option<String> {
    let missing_names: Vec<&String> = required_names
        .iter()
        .filter(|required| !output_names.contains(required))
        .collect();
    if missing_names.is_empty() {
        return None;
    }
    Some(format!(
        "cannot link {output} to {input}: {input} requires {missing_names:?}, which {output} \
         does not state (it states {output_names:?})"
    ))
}

#[cfg(feature = "contract-json-schema")]
mod contract_json_schema;
mod exact_pin_type;

#[cfg(test)]
mod tests;

#[cfg(test)]
mod exact_pin_type_tests;
