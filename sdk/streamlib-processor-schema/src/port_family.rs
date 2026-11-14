// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: GPL-3.0-only

//! The five port families a port declares, the keys each takes, and the
//! checks `connect` makes between an output and an input of one family.

use std::collections::BTreeSet;
use std::fmt;

use serde::{Deserialize, Serialize};

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
pub const IMAGE_TEXTURE_FORMAT_WIRE_NAMES: [&str; 7] = [
    "rgba8_unorm",
    "rgba8_unorm_srgb",
    "bgra8_unorm",
    "bgra8_unorm_srgb",
    "rgba16_float",
    "rgba32_float",
    "nv12",
];

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
        Some(serde_json::Value::String(wire_name)) => PortFamily::ALL
            .into_iter()
            .find(|family| family.wire_name() == wire_name)
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

/// The node a person places between `from` and `to` to convert one family to
/// the other.
fn conversion_between_families(from: PortFamily, to: PortFamily) -> &'static str {
    use PortFamily::*;
    match (from, to) {
        (Image, Array) => "place an ImageToArray node between them",
        (Array, Image) => "place an ArrayToImage node between them",
        (Channels, Array) => "place a ChannelsToArray node between them",
        (Array, Channels) => "place an ArrayToChannels node between them",
        (Channels, Records) => "place a ChannelsToRecords node between them",
        (Records, Channels) => "place a RecordsToChannels node between them",
        (Image, Packets) => "place a video encoder (H264Encoder, H265Encoder) between them",
        (Packets, Image) => "place a video decoder (H264Decoder, H265Decoder) between them",
        (Channels, Packets) => "place an OpusEncoder between them",
        (Packets, Channels) => "place an OpusDecoder between them",
        _ => "no one node converts between them; chain the conversion nodes that do",
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// `tatolab.stream` declares each family with the description the catalog
    /// serves, so its reference never says something the runtime does not.
    #[test]
    fn the_python_sdk_declares_each_family_with_the_description_the_runtime_serves() {
        let python_port_families = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../tatolab-stream/tatolab/stream/_port_family_declaration.py"
        ))
        .expect("the Python SDK's port family declarations are in this repository");
        for family in PortFamily::ALL {
            let declaration = format!(
                "PortFamily(\n    \"{}\",\n    description=\"{}\",\n)",
                family.declaration_name(),
                family.description()
            );
            assert!(
                python_port_families.contains(&declaration),
                "`tatolab.stream.{}` must be declared as `{declaration}`",
                family.declaration_name()
            );
        }
    }

    fn endpoint<'a>(
        node_name: &'a str,
        port_name: &'a str,
        declaration: &'a PortFamilyDeclaration,
    ) -> PortFamilyLinkEndpoint<'a> {
        PortFamilyLinkEndpoint {
            node_name,
            port_name,
            declaration,
        }
    }

    fn channels_output_stating(names: &[&str]) -> PortFamilyDeclaration {
        PortFamilyDeclaration {
            names: Some(OutputPortDeclaredNames::Stated(
                names.iter().map(|name| name.to_string()).collect(),
            )),
            ..PortFamilyDeclaration::of_family(PortFamily::Channels)
        }
    }

    fn channels_input_requiring(names: &[&str]) -> PortFamilyDeclaration {
        PortFamilyDeclaration {
            requires: names.iter().map(|name| name.to_string()).collect(),
            ..PortFamilyDeclaration::of_family(PortFamily::Channels)
        }
    }

    #[test]
    fn a_family_mismatch_is_refused_naming_both_endpoints_and_the_conversion() {
        let image = PortFamilyDeclaration::of_family(PortFamily::Image);
        let array = PortFamilyDeclaration::of_family(PortFamily::Array);
        let refusal = refusal_of_a_link_between_port_families(
            endpoint("camera", "video", &image),
            None,
            endpoint("model", "tensor", &array),
        )
        .expect("an Image output never feeds an Array input");
        assert!(refusal.contains("camera.video (Image)"), "{refusal}");
        assert!(refusal.contains("model.tensor (Array)"), "{refusal}");
        assert!(refusal.contains("ImageToArray"), "{refusal}");
    }

    #[test]
    fn a_link_of_one_family_with_nothing_required_is_taken() {
        let image = PortFamilyDeclaration::of_family(PortFamily::Image);
        assert_eq!(
            refusal_of_a_link_between_port_families(
                endpoint("camera", "video", &image),
                None,
                endpoint("display", "video", &image),
            ),
            None
        );
    }

    #[test]
    fn a_required_name_the_output_does_not_state_is_refused_naming_it() {
        let output = channels_output_stating(&["left", "right"]);
        let input = channels_input_requiring(&["left", "rpm"]);
        let refusal = refusal_of_a_link_between_port_families(
            endpoint("mic", "audio", &output),
            None,
            endpoint("meter", "audio", &input),
        )
        .expect("rpm is not stated");
        assert!(
            refusal.contains("requires [\"rpm\"], which"),
            "only the missing name: {refusal}"
        );
        assert!(refusal.contains("mic.audio") && refusal.contains("meter.audio"));
    }

    #[test]
    fn a_requirement_on_an_output_stating_no_names_is_refused() {
        let output = PortFamilyDeclaration::of_family(PortFamily::Channels);
        let input = channels_input_requiring(&["left"]);
        let refusal = refusal_of_a_link_between_port_families(
            endpoint("mic", "audio", &output),
            None,
            endpoint("meter", "audio", &input),
        )
        .expect("an output with no names satisfies no requirement");
        assert!(refusal.contains("states no names"), "{refusal}");
    }

    #[test]
    fn names_from_config_resolve_against_the_output_nodes_config() {
        let output = PortFamilyDeclaration {
            names: Some(OutputPortDeclaredNames::FromConfig {
                config_key: "channel_names".into(),
            }),
            ..PortFamilyDeclaration::of_family(PortFamily::Channels)
        };
        let input = channels_input_requiring(&["rpm"]);
        let config = serde_json::json!({ "channel_names": ["rpm", "torque"] });
        assert_eq!(
            refusal_of_a_link_between_port_families(
                endpoint("engine", "signals", &output),
                Some(&config),
                endpoint("gauge", "signals", &input),
            ),
            None
        );
        let config_without_rpm = serde_json::json!({ "channel_names": ["torque"] });
        assert!(
            refusal_of_a_link_between_port_families(
                endpoint("engine", "signals", &output),
                Some(&config_without_rpm),
                endpoint("gauge", "signals", &input),
            )
            .is_some()
        );
        let refusal = refusal_of_a_link_between_port_families(
            endpoint("engine", "signals", &output),
            None,
            endpoint("gauge", "signals", &input),
        )
        .expect("unset config key cannot satisfy rpm");
        assert!(refusal.contains("channel_names"), "{refusal}");
    }

    #[test]
    fn a_codec_outside_the_required_set_is_refused() {
        let output = PortFamilyDeclaration {
            codec: Some("h265".into()),
            ..PortFamilyDeclaration::of_family(PortFamily::Packets)
        };
        let input = PortFamilyDeclaration {
            requires_codec: vec!["h264".into()],
            ..PortFamilyDeclaration::of_family(PortFamily::Packets)
        };
        let refusal = refusal_of_a_link_between_port_families(
            endpoint("encoder", "packets", &output),
            None,
            endpoint("publisher", "video", &input),
        )
        .expect("h265 is not h264");
        assert!(refusal.contains("\"h265\"") && refusal.contains("\"h264\""));
        assert!(refusal.contains("encoder.packets") && refusal.contains("publisher.video"));
    }

    #[test]
    fn image_formats_that_do_not_overlap_are_refused() {
        let output = PortFamilyDeclaration {
            formats: vec!["rgba16_float".into()],
            ..PortFamilyDeclaration::of_family(PortFamily::Image)
        };
        let input = PortFamilyDeclaration {
            formats: vec!["rgba8_unorm".into()],
            ..PortFamilyDeclaration::of_family(PortFamily::Image)
        };
        let refusal = refusal_of_a_link_between_port_families(
            endpoint("render", "video", &output),
            None,
            endpoint("compositor", "video", &input),
        )
        .expect("rgba16_float never reaches an rgba8_unorm-only input");
        assert!(refusal.contains("rgba16_float") && refusal.contains("rgba8_unorm"));
        let input_reading_both = PortFamilyDeclaration {
            formats: vec!["rgba8_unorm".into(), "rgba16_float".into()],
            ..PortFamilyDeclaration::of_family(PortFamily::Image)
        };
        assert_eq!(
            refusal_of_a_link_between_port_families(
                endpoint("render", "video", &output),
                None,
                endpoint("compositor", "video", &input_reading_both),
            ),
            None
        );
    }

    #[test]
    fn an_image_outputs_frame_rate_resolves_from_its_class_or_its_nodes_config() {
        let stated = PortFamilyDeclaration {
            frame_rate: Some(ImageOutputPortFrameRate::Stated(
                crate::ImageStreamFrameRate {
                    numerator: 30_000,
                    denominator: 1_001,
                },
            )),
            ..PortFamilyDeclaration::of_family(PortFamily::Image)
        };
        assert_eq!(
            output_port_frame_rate_resolved_against_config(&stated, None),
            Ok(Some(crate::ImageStreamFrameRate {
                numerator: 30_000,
                denominator: 1_001
            }))
        );
        let from_config = PortFamilyDeclaration {
            frame_rate: Some(ImageOutputPortFrameRate::FromConfig {
                config_key: "frames_per_second".into(),
            }),
            ..PortFamilyDeclaration::of_family(PortFamily::Image)
        };
        let resolved = |config: serde_json::Value| {
            output_port_frame_rate_resolved_against_config(&from_config, Some(&config))
        };
        assert_eq!(
            resolved(serde_json::json!({"frames_per_second": 60})),
            Ok(Some(crate::ImageStreamFrameRate {
                numerator: 60,
                denominator: 1
            }))
        );
        assert_eq!(
            resolved(
                serde_json::json!({"frames_per_second": {"numerator": 24000, "denominator": 1001}})
            ),
            Ok(Some(crate::ImageStreamFrameRate {
                numerator: 24_000,
                denominator: 1_001
            }))
        );
        assert_eq!(resolved(serde_json::json!({})), Ok(None));
        assert!(resolved(serde_json::json!({"frames_per_second": 0})).is_err());
        assert!(resolved(serde_json::json!({"frames_per_second": "fast"})).is_err());
        assert!(
            PortFamilyDeclaration {
                frame_rate: from_config.frame_rate.clone(),
                ..PortFamilyDeclaration::of_family(PortFamily::Channels)
            }
            .refusal_of_its_keys("audio", PortFamilyDeclarationDirection::Output)
            .is_some_and(|refusal| refusal.contains("frame_rate"))
        );
    }

    #[test]
    fn a_format_that_is_no_engine_texture_name_is_refused() {
        let image_in_shorthand = PortFamilyDeclaration {
            formats: vec!["rgba8".into()],
            ..PortFamilyDeclaration::of_family(PortFamily::Image)
        };
        let refusal = image_in_shorthand
            .refusal_of_its_keys("video", PortFamilyDeclarationDirection::Output)
            .expect("rgba8 is no engine texture name");
        assert!(refusal.contains("\"rgba8\"") && refusal.contains("rgba8_unorm"));
    }

    #[test]
    fn a_key_the_family_or_direction_does_not_take_is_refused_by_name() {
        let image_with_names = PortFamilyDeclaration {
            names: Some(OutputPortDeclaredNames::Stated(vec!["x".into()])),
            ..PortFamilyDeclaration::of_family(PortFamily::Image)
        };
        let refusal = image_with_names
            .refusal_of_its_keys("video", PortFamilyDeclarationDirection::Output)
            .expect("Image has no names");
        assert!(refusal.contains("`names=`") && refusal.contains("Image"));
        let input_with_codec = PortFamilyDeclaration {
            codec: Some("h264".into()),
            ..PortFamilyDeclaration::of_family(PortFamily::Packets)
        };
        assert!(
            input_with_codec
                .refusal_of_its_keys("packets", PortFamilyDeclarationDirection::Input)
                .is_some()
        );
        assert_eq!(
            channels_output_stating(&["left"])
                .refusal_of_its_keys("audio", PortFamilyDeclarationDirection::Output),
            None
        );
    }

    #[test]
    fn a_declaration_round_trips_through_its_wire_form() {
        let declaration = PortFamilyDeclaration {
            names: Some(OutputPortDeclaredNames::FromConfig {
                config_key: "channel_names".into(),
            }),
            rate: Some(ChannelsOutputPortSampleRate::Stated(48_000)),
            dtype: Some(ChannelsOutputPortSampleType::F32),
            ..PortFamilyDeclaration::of_family(PortFamily::Channels)
        };
        let wire = serde_json::to_value(&declaration).unwrap();
        assert_eq!(wire["family"], "channels");
        assert_eq!(wire["names"]["from_config"]["config_key"], "channel_names");
        assert_eq!(
            serde_json::from_value::<PortFamilyDeclaration>(wire).unwrap(),
            declaration
        );
    }

    fn an_output_whose_family_and_codec_come_from_config() -> PortFamilyDeclaration {
        PortFamilyDeclaration {
            family_from_config_key: Some("family".into()),
            codec_from_config_key: Some("codec".into()),
            ..PortFamilyDeclaration::of_family(PortFamily::Packets)
        }
    }

    #[test]
    fn an_output_family_and_codec_from_config_resolve_against_the_output_nodes_config() {
        let output = an_output_whose_family_and_codec_come_from_config();
        let h264_input = PortFamilyDeclaration {
            requires_codec: vec!["h264".into()],
            ..PortFamilyDeclaration::of_family(PortFamily::Packets)
        };
        let records_input = PortFamilyDeclaration::of_family(PortFamily::Records);
        let source = endpoint("receive", "content", &output);

        let h264_config = serde_json::json!({"family": "packets", "codec": "h264"});
        assert_eq!(
            refusal_of_a_link_between_port_families(
                source,
                Some(&h264_config),
                endpoint("decoder", "encoded_video", &h264_input),
            ),
            None
        );

        let records_config = serde_json::json!({"family": "records"});
        assert_eq!(
            refusal_of_a_link_between_port_families(
                source,
                Some(&records_config),
                endpoint("sink", "records", &records_input),
            ),
            None
        );
        let refusal = refusal_of_a_link_between_port_families(
            source,
            Some(&records_config),
            endpoint("decoder", "encoded_video", &h264_input),
        )
        .expect("a Records output into a Packets input is refused");
        assert!(
            refusal.contains("receive.content (Records)") && refusal.contains("(Packets)"),
            "got {refusal}"
        );

        let opus_config = serde_json::json!({"family": "packets", "codec": "opus"});
        let refusal = refusal_of_a_link_between_port_families(
            source,
            Some(&opus_config),
            endpoint("decoder", "encoded_video", &h264_input),
        )
        .expect("a codec outside the required set is refused");
        assert!(refusal.contains("\"opus\""), "got {refusal}");

        let unknown_family = serde_json::json!({"family": "video"});
        let refusal = refusal_of_a_link_between_port_families(
            source,
            Some(&unknown_family),
            endpoint("sink", "records", &records_input),
        )
        .expect("a family no port has is refused");
        assert!(
            refusal.contains("\"video\"") && refusal.contains("records"),
            "got {refusal}"
        );
    }

    #[test]
    fn an_input_taking_several_families_takes_each_and_refuses_another_naming_the_conversion() {
        let input = PortFamilyDeclaration {
            also_takes_families: vec![PortFamily::Records],
            ..PortFamilyDeclaration::of_family(PortFamily::Packets)
        };
        let destination = endpoint("send", "content", &input);
        for family in [PortFamily::Packets, PortFamily::Records] {
            let output = PortFamilyDeclaration::of_family(family);
            assert_eq!(
                refusal_of_a_link_between_port_families(
                    endpoint("source", "out", &output),
                    None,
                    destination,
                ),
                None,
                "{family} is taken"
            );
        }
        let image = PortFamilyDeclaration::of_family(PortFamily::Image);
        let refusal = refusal_of_a_link_between_port_families(
            endpoint("camera", "video", &image),
            None,
            destination,
        )
        .expect("an Image link is refused");
        assert!(
            refusal.contains("(Packets or Records)") && refusal.contains("H264Encoder"),
            "got {refusal}"
        );
    }

    #[test]
    fn the_keys_resolved_from_config_are_taken_only_in_the_direction_that_has_them() {
        assert_eq!(
            an_output_whose_family_and_codec_come_from_config()
                .refusal_of_its_keys("content", PortFamilyDeclarationDirection::Output),
            None
        );
        assert!(
            an_output_whose_family_and_codec_come_from_config()
                .refusal_of_its_keys("content", PortFamilyDeclarationDirection::Input)
                .is_some_and(|refusal| refusal.contains("family_from_config_key"))
        );
        let input_taking_records = PortFamilyDeclaration {
            also_takes_families: vec![PortFamily::Records],
            ..PortFamilyDeclaration::of_family(PortFamily::Packets)
        };
        assert!(
            input_taking_records
                .refusal_of_its_keys("content", PortFamilyDeclarationDirection::Output)
                .is_some_and(|refusal| refusal.contains("also_takes_families"))
        );
        let records_output_with_a_codec_key = PortFamilyDeclaration {
            codec_from_config_key: Some("codec".into()),
            ..PortFamilyDeclaration::of_family(PortFamily::Records)
        };
        assert!(
            records_output_with_a_codec_key
                .refusal_of_its_keys("content", PortFamilyDeclarationDirection::Output)
                .is_some_and(|refusal| refusal.contains("codec_from_config_key"))
        );
    }

    #[test]
    fn a_records_output_whose_config_holds_an_empty_codec_links_to_a_records_input() {
        let records_input = PortFamilyDeclaration::of_family(PortFamily::Records);
        let config = serde_json::json!({"family": "records", "codec": ""});
        assert_eq!(
            refusal_of_a_link_between_port_families(
                endpoint(
                    "receive",
                    "content",
                    &an_output_whose_family_and_codec_come_from_config()
                ),
                Some(&config),
                endpoint("sink", "records", &records_input),
            ),
            None
        );
    }

    #[test]
    fn the_keys_of_every_family_a_port_may_carry_are_taken() {
        let input_taking_packets_and_records = PortFamilyDeclaration {
            also_takes_families: vec![PortFamily::Records],
            requires: vec!["pts".into()],
            requires_codec: vec!["h264".into()],
            ..PortFamilyDeclaration::of_family(PortFamily::Packets)
        };
        assert_eq!(
            input_taking_packets_and_records
                .refusal_of_its_keys("content", PortFamilyDeclarationDirection::Input),
            None
        );
        let output_whose_family_comes_from_config_stating_names = PortFamilyDeclaration {
            names: Some(OutputPortDeclaredNames::Stated(vec!["pts".into()])),
            ..an_output_whose_family_and_codec_come_from_config()
        };
        assert_eq!(
            output_whose_family_comes_from_config_stating_names
                .refusal_of_its_keys("content", PortFamilyDeclarationDirection::Output),
            None
        );
    }
}
